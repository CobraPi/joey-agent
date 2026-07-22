//! Tool-call result envelope (port of the result handling in
//! `_make_tool_handler`, mcp_tool.py:4187-4266, plus `_sanitize_error` and the
//! content-block renderers).
//!
//! Every `tools/call` outcome is rendered to a JSON string matching upstream's
//! `json.dumps(..., ensure_ascii=False)` output (including Python's default
//! `", "` / `": "` separators):
//!
//! * `isError` results  → `{"error": <sanitized text or fallback>}`
//! * text results       → `{"result": "<text blocks joined with \n>"}`
//! * structured results → `{"result": ..., "structuredContent": ...}` or
//!   `{"result": <structured>}` when no text is present
//! * exceptions         → `{"error": "MCP call failed: {Type}: {msg}"}`

use std::sync::OnceLock;

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Credential stripping (mcp_tool.py:388-401, 449-455)
// ---------------------------------------------------------------------------

fn credential_pattern() -> &'static regex::Regex {
    static P: OnceLock<regex::Regex> = OnceLock::new();
    P.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(?:ghp_[A-Za-z0-9_]{1,255}|sk-[A-Za-z0-9_]{1,255}|Bearer\s+\S+|token=[^\s&,;"']{1,255}|key=[^\s&,;"']{1,255}|API_KEY=[^\s&,;"']{1,255}|password=[^\s&,;"']{1,255}|secret=[^\s&,;"']{1,255})"#,
        )
        .expect("static regex")
    })
}

/// Strip credential-like patterns from error text before returning to the LLM
/// (`_sanitize_error`). Replaces tokens, keys, and other secrets with
/// `[REDACTED]`.
pub fn sanitize_error(text: &str) -> String {
    credential_pattern().replace_all(text, "[REDACTED]").into_owned()
}

// ---------------------------------------------------------------------------
// Python-compatible json.dumps
// ---------------------------------------------------------------------------

/// serde_json formatter matching Python's default `json.dumps` separators
/// (`", "` between items, `": "` after keys). Non-ASCII is written raw, which
/// matches `ensure_ascii=False`.
struct PyFormatter;

impl serde_json::ser::Formatter for PyFormatter {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        writer.write_all(b": ")
    }
}

/// `json.dumps(value, ensure_ascii=False)` with Python's default separators.
pub(crate) fn py_json_dumps(value: &Value) -> String {
    let mut out: Vec<u8> = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter);
    value.serialize(&mut ser).expect("serializing a Value cannot fail");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

/// `{"error": <sanitized text>}` envelope.
pub(crate) fn error_envelope(text: &str) -> String {
    py_json_dumps(&json!({ "error": sanitize_error(text) }))
}

// ---------------------------------------------------------------------------
// Content-block rendering (mcp_tool.py:714-918)
// ---------------------------------------------------------------------------

/// Hard cap on decoded resource bytes materialized from an MCP tool result
/// (`_MCP_RESOURCE_MAX_BYTES`).
const MCP_RESOURCE_MAX_BYTES: usize = 50 * 1024 * 1024;
/// Base64 expands raw bytes by ~4/3; reject oversized payloads before
/// decoding (`_MCP_RESOURCE_MAX_B64_CHARS`).
const MCP_RESOURCE_MAX_B64_CHARS: usize = MCP_RESOURCE_MAX_BYTES * 4 / 3 + 4;

/// `str(mime or "").split(";", 1)[0].strip().lower()`.
fn normalize_mime(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// Base64 decode with Python's `b64decode` leniency (non-alphabet characters
/// are discarded before decoding).
fn b64decode_forgiving(data: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let filtered: String = data
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        .collect();
    base64::engine::general_purpose::STANDARD.decode(filtered)
}

fn get_str<'v>(block: &'v Value, key: &str) -> Option<&'v str> {
    block.get(key).and_then(Value::as_str)
}

/// `_cache_mcp_image_block` in a process without the gateway image cache:
/// image blocks always render empty (the decode is still attempted so
/// malformed payloads are logged, matching upstream).
fn render_image_block(block: &Value) -> String {
    let data = get_str(block, "data");
    let mime = normalize_mime(block.get("mimeType"));
    let Some(data) = data else { return String::new() };
    if !mime.starts_with("image/") {
        return String::new();
    }
    if b64decode_forgiving(data).is_err() {
        warn!("MCP image block decode failed ({}): invalid base64", mime);
        return String::new();
    }
    // Upstream caches the bytes via gateway.platforms.base and returns a
    // MEDIA: tag; without the gateway it falls back to dropping the block.
    debug!("MCP image caching skipped — gateway image cache unavailable");
    String::new()
}

/// `_cache_mcp_audio_block` in a process without the gateway audio cache: the
/// size-cap markers are still produced (they precede the cache), everything
/// else renders empty.
fn render_audio_block(block: &Value) -> String {
    let data = get_str(block, "data");
    let mime = normalize_mime(block.get("mimeType"));
    let Some(data) = data else { return String::new() };
    if !mime.starts_with("audio/") {
        return String::new();
    }
    if data.len() > MCP_RESOURCE_MAX_B64_CHARS {
        return format!("[MCP audio resource too large to cache: ~{} bytes]", data.len() * 3 / 4);
    }
    let raw = match b64decode_forgiving(data) {
        Ok(raw) => raw,
        Err(_) => {
            warn!("MCP audio block decode failed ({}): invalid base64", mime);
            return String::new();
        }
    };
    if raw.len() > MCP_RESOURCE_MAX_BYTES {
        return format!("[MCP audio resource too large to cache: {} bytes]", raw.len());
    }
    debug!("MCP audio caching skipped — gateway audio cache unavailable");
    String::new()
}

/// Render an MCP `ResourceLink` or `EmbeddedResource` block as text
/// (`_render_mcp_resource_block`). Blob resources cannot be materialized
/// without the gateway document cache, so they take upstream's
/// "cache unavailable in this process" path.
fn render_resource_block(block: &Value, server_name: &str) -> String {
    let block_type = get_str(block, "type").unwrap_or("");

    if block_type == "resource_link"
        || (block.get("uri").is_some() && block.get("resource").is_none() && block_type != "text")
    {
        let uri = get_str(block, "uri").unwrap_or("");
        if uri.is_empty() {
            return String::new();
        }
        let name = get_str(block, "name").unwrap_or("");
        let mime = get_str(block, "mimeType").unwrap_or("");
        let mut details = format!("uri={}", uri);
        if !name.is_empty() {
            details.push_str(&format!(", name={}", name));
        }
        if !mime.is_empty() {
            details.push_str(&format!(", mimeType={}", mime));
        }
        let reader = if !server_name.is_empty() {
            crate::mcp_prefixed_tool_name(server_name, "read_resource")
        } else {
            "the MCP server's read_resource tool".to_string()
        };
        return format!("[MCP resource link: {} — fetch it with {}]", details, reader);
    }

    let Some(resource) = block.get("resource") else {
        return String::new();
    };

    match resource.get("text") {
        Some(Value::Null) | None => {}
        Some(Value::String(text)) => return text.clone(),
        Some(other) => return py_json_dumps(other),
    }

    let Some(blob) = resource.get("blob").and_then(Value::as_str) else {
        return String::new();
    };

    let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
    let mime = resource.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let mime_or_uri = if !mime.is_empty() { mime } else { uri };
    if blob.len() > MCP_RESOURCE_MAX_B64_CHARS {
        return format!(
            "[MCP embedded resource too large to cache: ~{} bytes, uri={}]",
            blob.len() * 3 / 4,
            uri
        );
    }
    let raw = match b64decode_forgiving(blob) {
        Ok(raw) => raw,
        Err(_) => {
            warn!("MCP embedded resource decode failed ({}): invalid base64", mime_or_uri);
            return format!("[MCP embedded resource could not be decoded: {}]", mime_or_uri);
        }
    };
    if raw.len() > MCP_RESOURCE_MAX_BYTES {
        return format!(
            "[MCP embedded resource too large to cache: {} bytes, uri={}]",
            raw.len(),
            uri
        );
    }
    debug!("MCP resource caching skipped — gateway document cache unavailable");
    format!(
        "[MCP embedded resource received ({} bytes, {}) but document cache unavailable in this process]",
        raw.len(),
        if mime.is_empty() { "unknown type" } else { mime }
    )
}

// ---------------------------------------------------------------------------
// The tools/call result envelope
// ---------------------------------------------------------------------------

/// Render a successful `tools/call` JSON-RPC result into the model-visible
/// envelope (mcp_tool.py:4187-4266).
pub(crate) fn render_call_result(server_name: &str, result: &Value) -> String {
    let content: &[Value] = result
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // MCP CallToolResult has .content (list of content blocks) and .isError.
    if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        let mut error_text = String::new();
        for block in content {
            if let Some(text) = get_str(block, "text").filter(|t| !t.is_empty()) {
                error_text.push_str(text);
                continue;
            }
            // EmbeddedResource blocks inside error payloads carry their text
            // under .resource.text.
            if let Some(res_text) = block
                .get("resource")
                .and_then(|r| r.get("text"))
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                error_text.push_str(res_text);
            }
        }
        let text = if error_text.is_empty() { "MCP tool returned an error" } else { &error_text };
        return error_envelope(text);
    }

    // Collect text from content blocks. Image/audio caching and blob resource
    // materialization need the gateway media cache (not ported); those blocks
    // follow upstream's gateway-less fallthrough.
    let mut parts: Vec<String> = Vec::new();
    for block in content {
        if let Some(text) = get_str(block, "text").filter(|t| !t.is_empty()) {
            parts.push(text.to_string());
            continue;
        }
        let image = render_image_block(block);
        if !image.is_empty() {
            parts.push(image);
            continue;
        }
        let audio = render_audio_block(block);
        if !audio.is_empty() {
            parts.push(audio);
            continue;
        }
        let resource_text = render_resource_block(block, server_name);
        if !resource_text.is_empty() {
            parts.push(resource_text);
            continue;
        }
        // Benign empty renders aren't data loss — log at debug. Warn only for
        // genuinely unrecognized block shapes.
        let block_type = get_str(block, "type").unwrap_or("object");
        if matches!(block_type, "text" | "resource" | "audio" | "image") {
            debug!("MCP {}: content block type '{}' rendered empty", server_name, block_type);
        } else {
            warn!("MCP {}: dropping unsupported content block type '{}'", server_name, block_type);
        }
    }
    let text_result = parts.join("\n");

    // Combine content + structuredContent when both are present. MCP spec:
    // content is model-oriented (text), structuredContent is machine-oriented
    // (JSON metadata).
    match result.get("structuredContent") {
        Some(structured) if !structured.is_null() => {
            if !text_result.is_empty() {
                py_json_dumps(&json!({
                    "result": text_result,
                    "structuredContent": structured,
                }))
            } else {
                py_json_dumps(&json!({ "result": structured }))
            }
        }
        _ => py_json_dumps(&json!({ "result": text_result })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_credentials() {
        assert_eq!(sanitize_error("token ghp_abc123 leaked"), "token [REDACTED] leaked");
        assert_eq!(sanitize_error("Authorization: Bearer xyz.abc"), "Authorization: [REDACTED]");
        assert_eq!(sanitize_error("url?api_key=123&x=1"), "url?[REDACTED]&x=1");
        assert_eq!(sanitize_error("PASSWORD=hunter2;"), "[REDACTED];");
        assert_eq!(sanitize_error("sk-proj-abc"), "[REDACTED]-abc");
        assert_eq!(sanitize_error("nothing to see"), "nothing to see");
    }

    #[test]
    fn py_json_dumps_matches_python_separators() {
        assert_eq!(py_json_dumps(&json!({"error": "x"})), r#"{"error": "x"}"#);
        assert_eq!(py_json_dumps(&json!({"a": [1, 2], "b": "ü"})), r#"{"a": [1, 2], "b": "ü"}"#);
        assert_eq!(py_json_dumps(&json!({"result": "a\nb"})), "{\"result\": \"a\\nb\"}");
    }

    #[test]
    fn is_error_collects_text_and_resource_text() {
        let result = json!({
            "isError": true,
            "content": [
                {"type": "text", "text": "bad "},
                {"type": "resource", "resource": {"uri": "x://y", "text": "details"}}
            ]
        });
        assert_eq!(render_call_result("srv", &result), r#"{"error": "bad details"}"#);
    }

    #[test]
    fn is_error_empty_content_uses_fallback() {
        let result = json!({"isError": true, "content": []});
        assert_eq!(
            render_call_result("srv", &result),
            r#"{"error": "MCP tool returned an error"}"#
        );
    }

    #[test]
    fn is_error_output_is_sanitized() {
        let result = json!({
            "isError": true,
            "content": [{"type": "text", "text": "auth failed: Bearer supersecret"}]
        });
        assert_eq!(
            render_call_result("srv", &result),
            r#"{"error": "auth failed: [REDACTED]"}"#
        );
    }

    #[test]
    fn plain_text_blocks_join_with_newline() {
        let result = json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "world"}
            ]
        });
        assert_eq!(render_call_result("srv", &result), "{\"result\": \"hello\\nworld\"}");
    }

    #[test]
    fn empty_content_yields_empty_result_string() {
        let result = json!({"content": []});
        assert_eq!(render_call_result("srv", &result), r#"{"result": ""}"#);
    }

    #[test]
    fn structured_content_with_text() {
        let result = json!({
            "content": [{"type": "text", "text": "ok"}],
            "structuredContent": {"count": 2}
        });
        assert_eq!(
            render_call_result("srv", &result),
            r#"{"result": "ok", "structuredContent": {"count": 2}}"#
        );
    }

    #[test]
    fn structured_content_without_text() {
        let result = json!({"content": [], "structuredContent": {"count": 2}});
        assert_eq!(render_call_result("srv", &result), r#"{"result": {"count": 2}}"#);
    }

    #[test]
    fn null_structured_content_is_ignored() {
        let result = json!({"content": [{"type": "text", "text": "t"}], "structuredContent": null});
        assert_eq!(render_call_result("srv", &result), r#"{"result": "t"}"#);
    }

    #[test]
    fn embedded_text_resource_renders_in_success_path() {
        let result = json!({
            "content": [{"type": "resource", "resource": {"uri": "doc://1", "text": "file body"}}]
        });
        assert_eq!(render_call_result("srv", &result), r#"{"result": "file body"}"#);
    }

    #[test]
    fn resource_link_renders_pointer() {
        let result = json!({
            "content": [{
                "type": "resource_link",
                "uri": "doc://1",
                "name": "spec",
                "mimeType": "application/pdf"
            }]
        });
        assert_eq!(
            render_call_result("myserver", &result),
            "{\"result\": \"[MCP resource link: uri=doc://1, name=spec, mimeType=application/pdf — fetch it with mcp__myserver__read_resource]\"}"
        );
    }

    #[test]
    fn blob_resource_reports_cache_unavailable() {
        let blob = base64::engine::general_purpose::STANDARD.encode(b"12345678");
        let result = json!({
            "content": [{
                "type": "resource",
                "resource": {"uri": "doc://1", "mimeType": "application/pdf", "blob": blob}
            }]
        });
        assert_eq!(
            render_call_result("srv", &result),
            r#"{"result": "[MCP embedded resource received (8 bytes, application/pdf) but document cache unavailable in this process]"}"#
        );
    }

    #[test]
    fn unsupported_blocks_are_dropped_not_dumped() {
        let result = json!({
            "content": [
                {"type": "text", "text": "keep"},
                {"type": "bogus_block", "payload": "zzz"}
            ]
        });
        assert_eq!(render_call_result("srv", &result), r#"{"result": "keep"}"#);
    }

    #[test]
    fn oversized_audio_marker() {
        // Fake an oversized base64 payload without allocating 70MB: length
        // check happens before decoding.
        let data = "A".repeat(MCP_RESOURCE_MAX_B64_CHARS + 1);
        let block = json!({"type": "audio", "data": data, "mimeType": "audio/wav"});
        let expected = format!(
            "[MCP audio resource too large to cache: ~{} bytes]",
            (MCP_RESOURCE_MAX_B64_CHARS + 1) * 3 / 4
        );
        assert_eq!(render_audio_block(&block), expected);
    }
}
