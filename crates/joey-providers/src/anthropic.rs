//! Anthropic Messages wire adapter — port of `agent/anthropic_adapter.py`
//! (message/tool conversion, extended-thinking contract, history shaping,
//! prompt caching, model-name normalization) and the normalization side of
//! `agent/transports/anthropic.py`.
//!
//! NOTE ON UPSTREAM'S OAUTH COMPATIBILITY LAYER: upstream's Anthropic-OAuth
//! path impersonates Claude Code so subscription billing accepts third-party
//! traffic (an injected "You are Claude Code…" system prefix, product-name
//! rewriting, `mcp__` tool renaming, a spoofed claude-code user-agent, and
//! OAuth-only beta headers — anthropic_adapter.py:343-420, 2553-2616). That
//! identity/spoofing layer is deliberately NOT replicated here: it exists to
//! evade Anthropic's ToS-side billing classification. The honest part —
//! detecting OAuth-shaped tokens to pick `Authorization: Bearer` over
//! `x-api-key` — is kept (see [`is_oauth_token`]).

use serde_json::{json, Map, Value};

use crate::request::{ProviderRequest, ReasoningEffort};
use crate::types::{FinishReason, NormalizedResponse, ToolCall, ToolSchema, Usage};

// ── Extended-thinking contract (anthropic_adapter.py:58-121) ────────────────

/// Manual thinking budgets by effort (anthropic_adapter.py:58). Unknown
/// efforts fall back to 8000 (`THINKING_BUDGET.get(effort, 8000)`).
fn thinking_budget(effort: &str) -> u64 {
    match effort {
        "xhigh" => 32000,
        "high" => 16000,
        "medium" => 8000,
        "low" => 4000,
        _ => 8000,
    }
}

/// Hermes effort → adaptive `output_config.effort` (anthropic_adapter.py:67-75).
/// Unknown efforts map to "medium" (`ADAPTIVE_EFFORT_MAP.get(effort, "medium")`).
fn adaptive_effort(effort: &str) -> &'static str {
    match effort {
        "ultra" | "max" => "max",
        "xhigh" => "xhigh",
        "high" => "high",
        "medium" => "medium",
        "low" | "minimal" => "low",
        _ => "medium",
    }
}

/// Older Claude families that require manual budget-based thinking
/// (anthropic_adapter.py:98-106). Substring-matched; unknown Claude models
/// default to the modern adaptive contract.
const LEGACY_MANUAL_THINKING_CLAUDE_SUBSTRINGS: &[&str] = &[
    "claude-3", // 3, 3.5, 3.7
    "claude-opus-4-0",
    "claude-opus-4.0",
    "claude-opus-4-1",
    "claude-opus-4.1",
    "claude-sonnet-4-0",
    "claude-sonnet-4.0",
    "claude-opus-4-2025",
    "claude-sonnet-4-2025", // date-stamped 4.0 IDs
    "claude-opus-4-5",
    "claude-opus-4.5",
    "claude-sonnet-4-5",
    "claude-sonnet-4.5",
    "claude-haiku-4-5",
    "claude-haiku-4.5",
];

/// Adaptive models that do NOT accept the "xhigh" effort level
/// (anthropic_adapter.py:111-114): the 4.6 family only.
const NO_XHIGH_CLAUDE_SUBSTRINGS: &[&str] = &[
    "claude-opus-4-6",
    "claude-opus-4.6",
    "claude-sonnet-4-6",
    "claude-sonnet-4.6",
];

fn is_claude_model(model: &str) -> bool {
    model.to_lowercase().contains("claude")
}

/// Kimi / Moonshot family detection (anthropic_adapter.py:465-491).
const KIMI_FAMILY_MODEL_PREFIXES: &[&str] = &[
    "kimi-", "kimi_", "moonshot-", "moonshot_", "k1.", "k1-", "k2.", "k2-", "k25", "k2.5", "k3.",
    "k3-",
];
const KIMI_FAMILY_EXACT_SLUGS: &[&str] = &["k3"];

pub(crate) fn model_name_is_kimi_family(model: &str) -> bool {
    let mut m = model.trim().to_lowercase();
    if m.is_empty() {
        return false;
    }
    if let Some((_, tail)) = m.rsplit_once('/') {
        m = tail.to_string();
    }
    if KIMI_FAMILY_EXACT_SLUGS.contains(&m.as_str()) {
        return true;
    }
    KIMI_FAMILY_MODEL_PREFIXES.iter().any(|p| m.starts_with(p))
}

/// True for models using the adaptive-thinking contract (Claude 4.6+,
/// unknown-Claude-defaults-to-adaptive, Kimi family)
/// (anthropic_adapter.py:245-262).
pub(crate) fn supports_adaptive_thinking(model: &str) -> bool {
    if model_name_is_kimi_family(model) {
        return true;
    }
    if !is_claude_model(model) {
        return false;
    }
    let m = model.to_lowercase();
    !LEGACY_MANUAL_THINKING_CLAUDE_SUBSTRINGS.iter().any(|v| m.contains(v))
}

/// True when the model accepts the "xhigh" adaptive effort level (4.7+)
/// (anthropic_adapter.py:265-279).
fn supports_xhigh_effort(model: &str) -> bool {
    if !supports_adaptive_thinking(model) {
        return false;
    }
    let m = model.to_lowercase();
    !NO_XHIGH_CLAUDE_SUBSTRINGS.iter().any(|v| m.contains(v))
}

/// True for models that 400 on any non-default temperature/top_p/top_k
/// (4.7+; anthropic_adapter.py:282-298).
fn forbids_sampling_params(model: &str) -> bool {
    if !is_claude_model(model) {
        return false;
    }
    let m = model.to_lowercase();
    if NO_XHIGH_CLAUDE_SUBSTRINGS.iter().any(|v| m.contains(v)) {
        return false; // 4.6 family is adaptive but still accepts sampling params
    }
    !LEGACY_MANUAL_THINKING_CLAUDE_SUBSTRINGS.iter().any(|v| m.contains(v))
}

// ── Max output token limits (anthropic_adapter.py:127-185) ──────────────────

const ANTHROPIC_OUTPUT_LIMITS: &[(&str, u32)] = &[
    ("claude-fable", 128_000),
    ("claude-sonnet-5", 128_000),
    ("claude-opus-4-8", 128_000),
    ("claude-opus-4-7", 128_000),
    ("claude-opus-4-6", 128_000),
    ("claude-sonnet-4-6", 64_000),
    ("claude-opus-4-5", 64_000),
    ("claude-sonnet-4-5", 64_000),
    ("claude-haiku-4-5", 64_000),
    ("claude-opus-4", 32_000),
    ("claude-sonnet-4", 64_000),
    ("claude-3-7-sonnet", 128_000),
    ("claude-3-5-sonnet", 8_192),
    ("claude-3-5-haiku", 8_192),
    ("claude-3-opus", 4_096),
    ("claude-3-sonnet", 4_096),
    ("claude-3-haiku", 4_096),
    ("minimax", 131_072),
    ("qwen3", 65_536),
];

/// For models not in the table, assume the highest current limit
/// (anthropic_adapter.py:164).
const ANTHROPIC_DEFAULT_OUTPUT_LIMIT: u32 = 128_000;

/// Longest-substring-match output ceiling for an Anthropic model
/// (`_get_anthropic_max_output`, anthropic_adapter.py:167-185).
pub(crate) fn get_anthropic_max_output(model: &str) -> u32 {
    let m = model.to_lowercase().replace('.', "-");
    let mut best_key_len = 0usize;
    let mut best_val = ANTHROPIC_DEFAULT_OUTPUT_LIMIT;
    for (key, val) in ANTHROPIC_OUTPUT_LIMITS {
        if m.contains(key) && key.len() > best_key_len {
            best_key_len = key.len();
            best_val = *val;
        }
    }
    best_val
}

/// True when the chat-wire model is Anthropic-family enough to need the
/// max-output fallback (chat_completion_helpers.py:1134-1142: gated on
/// `_ANTHROPIC_OUTPUT_LIMITS` key membership).
pub(crate) fn model_in_anthropic_output_table(model: &str) -> bool {
    let m = model.to_lowercase().replace('.', "-");
    ANTHROPIC_OUTPUT_LIMITS.iter().any(|(key, _)| m.contains(key))
}

/// Resolve the mandatory Anthropic `max_tokens` (anthropic_adapter.py:214-242):
/// caller value when positive, else the per-model output ceiling.
fn resolve_anthropic_max_tokens(requested: Option<u32>, model: &str) -> u32 {
    match requested {
        Some(v) if v > 0 => v,
        _ => get_anthropic_max_output(model),
    }
}

// ── Model-name normalization (anthropic_adapter.py:1585-1631) ───────────────

/// Detect AWS Bedrock model IDs whose dots are namespace separators.
fn is_bedrock_model_id(model: &str) -> bool {
    let lower = model.to_lowercase();
    const REGIONAL: &[&str] = &[
        "global.", "us.", "eu.", "apac.", "ap.", "au.", "jp.", "ca.", "sa.", "me.", "af.",
    ];
    if REGIONAL.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    lower.starts_with("anthropic.")
}

/// Normalize a model name for the Anthropic API
/// (`normalize_model_name`, anthropic_adapter.py:1605-1631):
/// - strips the `anthropic/` prefix (case-insensitive)
/// - dots→hyphens for `claude-*` models (claude-opus-4.6 → claude-opus-4-6)
/// - preserves Bedrock IDs (`anthropic.claude-*`, `us.anthropic.claude-*`)
/// - leaves non-Anthropic models untouched (gpt-5.4, gemini-2.5, …)
pub fn normalize_model_name(model: &str) -> String {
    let mut model = model.to_string();
    if model.to_lowercase().starts_with("anthropic/") {
        model = model["anthropic/".len()..].to_string();
    }
    if is_bedrock_model_id(&model) {
        return model;
    }
    let lower = model.to_lowercase();
    if lower.starts_with("claude-") || lower.starts_with("anthropic/") {
        model = model.replace('.', "-");
    }
    model
}

// ── OAuth-token detection (anthropic_adapter.py:395-420) ────────────────────

/// Positively identify Anthropic OAuth/setup tokens by key format, choosing
/// `Authorization: Bearer` over `x-api-key`:
/// - `sk-ant-api*` → regular Console keys, never OAuth
/// - `sk-ant-*` (any other) → setup tokens / managed keys
/// - `eyJ*` → JWTs from the Anthropic OAuth flow
/// - `cc-*` → Claude Code OAuth access tokens
pub(crate) fn is_oauth_token(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if key.starts_with("sk-ant-api") {
        return false;
    }
    key.starts_with("sk-ant-") || key.starts_with("eyJ") || key.starts_with("cc-")
}

// ── Beta headers (anthropic_adapter.py:326-341, 573-648) ────────────────────

const COMMON_BETAS: &[&str] = &[
    "interleaved-thinking-2025-05-14",
    "fine-grained-tool-streaming-2025-05-14",
];
const TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";

fn is_minimax_anthropic_endpoint(base_url: &str) -> bool {
    let n = base_url.trim().trim_end_matches('/').to_lowercase();
    n.starts_with("https://api.minimax.io/anthropic") || n.starts_with("https://api.minimaxi.com/anthropic")
}

/// The `anthropic-beta` header value for the configured endpoint
/// (`_common_betas_for_base_url`, anthropic_adapter.py:619-648). The
/// 1M-context beta is endpoint-conditional: only Azure hosts still gate 1M
/// context behind it; native Anthropic rejects it on some subscriptions.
/// The OAuth-only betas (`claude-code-*`, `oauth-*`) are deliberately not
/// sent — see the module note on the unreplicated compatibility layer.
pub(crate) fn anthropic_beta_header(base_url: &str) -> Option<String> {
    let mut betas: Vec<&str> = COMMON_BETAS.to_vec();
    if base_url.to_lowercase().contains("azure.com") {
        betas.push(CONTEXT_1M_BETA);
    }
    if is_minimax_anthropic_endpoint(base_url) {
        betas.retain(|b| *b != TOOL_STREAMING_BETA && *b != CONTEXT_1M_BETA);
    }
    if betas.is_empty() {
        None
    } else {
        Some(betas.join(","))
    }
}

// ── Tool sanitization (anthropic_adapter.py:1634-1723) ──────────────────────

/// Sanitize a tool call ID to `[a-zA-Z0-9_-]` (anthropic_adapter.py:1634-1644).
pub(crate) fn sanitize_tool_id(tool_id: &str) -> String {
    if tool_id.is_empty() {
        return "tool_0".to_string();
    }
    let out: String = tool_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if out.is_empty() {
        "tool_0".to_string()
    } else {
        out
    }
}

/// Collapse `anyOf`/`oneOf` nullable unions to the non-null branch
/// (tools/schema_sanitizer.strip_nullable_unions with keep_nullable_hint=False).
fn strip_nullable_unions(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(strip_nullable_unions).collect()),
        Value::Object(map) => {
            let stripped: Map<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), strip_nullable_unions(v)))
                .collect();
            for key in ["anyOf", "oneOf"] {
                let Some(Value::Array(variants)) = stripped.get(key) else {
                    continue;
                };
                let non_null: Vec<&Value> = variants
                    .iter()
                    .filter(|item| {
                        !(item.is_object()
                            && item.get("type").and_then(|t| t.as_str()) == Some("null"))
                    })
                    .collect();
                if non_null.len() == 1 && non_null.len() != variants.len() {
                    let mut replacement = if non_null[0].is_object() {
                        non_null[0].as_object().unwrap().clone()
                    } else {
                        Map::new()
                    };
                    for meta_key in ["title", "description", "default", "examples"] {
                        if let Some(v) = stripped.get(meta_key) {
                            if !replacement.contains_key(meta_key) {
                                // `default` is illegal alongside `$ref` on strict backends.
                                if meta_key == "default" && replacement.contains_key("$ref") {
                                    continue;
                                }
                                replacement.insert(meta_key.to_string(), v.clone());
                            }
                        }
                    }
                    return strip_nullable_unions(&Value::Object(replacement));
                }
            }
            Value::Object(stripped)
        }
        other => other.clone(),
    }
}

/// Normalize a tool input schema for Anthropic
/// (`_normalize_tool_input_schema`, anthropic_adapter.py:1647-1685):
/// nullable-union collapse, top-level `oneOf`/`allOf`/`anyOf` removal, and a
/// guaranteed `{type: object, properties: {}}` floor.
fn normalize_tool_input_schema(schema: &Value) -> Value {
    let empty_schema = json!({"type": "object", "properties": {}});
    let is_empty = match schema {
        Value::Null => true,
        Value::Object(m) => m.is_empty(),
        _ => false,
    };
    if is_empty {
        return empty_schema;
    }
    let normalized = strip_nullable_unions(schema);
    let Value::Object(mut map) = normalized else {
        return empty_schema;
    };
    let banned = ["oneOf", "allOf", "anyOf"];
    if banned.iter().any(|k| map.contains_key(*k)) {
        for k in banned {
            map.remove(k);
        }
        if !map.contains_key("type") {
            map.insert("type".into(), json!("object"));
        }
    }
    if map.get("type").and_then(|t| t.as_str()) == Some("object")
        && !map.get("properties").map(|p| p.is_object()).unwrap_or(false)
    {
        map.insert("properties".into(), json!({}));
    }
    Value::Object(map)
}

/// Convert OpenAI tool definitions to Anthropic format with duplicate-name
/// dedup and `cache_control` forwarding (anthropic_adapter.py:1688-1723).
pub(crate) fn convert_tools_to_anthropic(tools: &[ToolSchema]) -> Vec<Value> {
    let mut result = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for t in tools {
        let name = t.function.name.as_str();
        if !name.is_empty() && !seen.insert(name) {
            tracing::warn!("duplicate tool name '{name}' — dropping second occurrence");
            continue;
        }
        let mut tool = json!({
            "name": name,
            "description": t.function.description,
            "input_schema": normalize_tool_input_schema(&t.function.parameters),
        });
        if let Some(cc) = &t.cache_control {
            if cc.is_object() {
                tool["cache_control"] = cc.clone();
            }
        }
        result.push(tool);
    }
    result
}

// ── Content-part conversion (anthropic_adapter.py:1726-1881) ────────────────

/// OpenAI image URL / data URL → Anthropic image source
/// (`_image_source_from_openai_url`, anthropic_adapter.py:1726-1745).
/// Media type defaults to image/jpeg; only `image/*` MIME parts are accepted.
fn image_source_from_openai_url(url: &str) -> Value {
    let url = url.trim();
    if url.is_empty() {
        return json!({"type": "url", "url": ""});
    }
    if let Some(rest) = url.strip_prefix("data:") {
        let (header, data) = rest.split_once(',').unwrap_or((rest, ""));
        let mut media_type = "image/jpeg";
        let mime_part = header.split(';').next().unwrap_or("").trim();
        if mime_part.starts_with("image/") {
            media_type = mime_part;
        }
        return json!({"type": "base64", "media_type": media_type, "data": data});
    }
    json!({"type": "url", "url": url})
}

/// One OpenAI-style content part → Anthropic block
/// (`_convert_content_part_to_anthropic`, anthropic_adapter.py:1748-1779).
fn convert_content_part_to_anthropic(part: &Value) -> Option<Value> {
    if part.is_null() {
        return None;
    }
    if let Some(s) = part.as_str() {
        return Some(json!({"type": "text", "text": s}));
    }
    let Some(obj) = part.as_object() else {
        return Some(json!({"type": "text", "text": part.to_string()}));
    };
    let ptype = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let mut block = match ptype {
        "input_text" => json!({"type": "text", "text": obj.get("text").and_then(|t| t.as_str()).unwrap_or("")}),
        "text" => {
            // Rebuild from whitelisted fields only — SDK output-only siblings
            // (parsed_output, citations=None) are rejected as request input.
            let mut b = json!({"type": "text", "text": obj.get("text").and_then(|t| t.as_str()).unwrap_or("")});
            if let Some(Value::Array(cits)) = obj.get("citations") {
                if !cits.is_empty() {
                    b["citations"] = Value::Array(cits.clone());
                }
            }
            b
        }
        "image_url" | "input_image" => {
            let url = match obj.get("image_url") {
                Some(Value::Object(iu)) => iu.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            json!({"type": "image", "source": image_source_from_openai_url(&url)})
        }
        _ => Value::Object(obj.clone()),
    };
    if let Some(cc @ Value::Object(_)) = obj.get("cache_control") {
        if block.get("cache_control").is_none() {
            block["cache_control"] = cc.clone();
        }
    }
    Some(block)
}

fn convert_content_to_anthropic(content: &Value) -> Value {
    let Some(items) = content.as_array() else {
        return content.clone();
    };
    Value::Array(items.iter().filter_map(convert_content_part_to_anthropic).collect())
}

/// OpenAI-style tool-message content parts → Anthropic tool_result inner
/// blocks (text + image only; `_content_parts_to_anthropic_blocks`,
/// anthropic_adapter.py:1858-1881).
fn content_parts_to_anthropic_blocks(parts: &Value) -> Vec<Value> {
    let Some(items) = parts.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in items {
        let Some(block) = convert_content_part_to_anthropic(part) else {
            continue;
        };
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        out.push(json!({"type": "text", "text": text}));
                    }
                }
            }
            Some("image") => {
                if let Some(src @ Value::Object(m)) = block.get("source") {
                    if !m.is_empty() {
                        out.push(json!({"type": "image", "source": src.clone()}));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ── Replay-block sanitization (anthropic_adapter.py:1884-1933) ──────────────

/// Whitelist-rebuild a stored Anthropic content block so it is valid REQUEST
/// input on replay (`_sanitize_replay_block`).
pub(crate) fn sanitize_replay_block(b: &Value) -> Option<Value> {
    let obj = b.as_object()?;
    match obj.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            let mut out = json!({"type": "text", "text": obj.get("text").and_then(|t| t.as_str()).unwrap_or("")});
            if let Some(Value::Array(cits)) = obj.get("citations") {
                if !cits.is_empty() {
                    out["citations"] = Value::Array(cits.clone());
                }
            }
            if let Some(cc @ Value::Object(_)) = obj.get("cache_control") {
                out["cache_control"] = cc.clone();
            }
            Some(out)
        }
        Some("thinking") => {
            let mut out = json!({"type": "thinking", "thinking": obj.get("thinking").and_then(|t| t.as_str()).unwrap_or("")});
            if let Some(sig) = obj.get("signature").and_then(|s| s.as_str()) {
                if !sig.is_empty() {
                    out["signature"] = json!(sig);
                }
            }
            Some(out)
        }
        Some("redacted_thinking") => {
            // Only valid with its data payload; drop if missing.
            let data = obj.get("data").and_then(|d| d.as_str())?;
            if data.is_empty() {
                return None;
            }
            Some(json!({"type": "redacted_thinking", "data": data}))
        }
        Some("tool_use") => {
            let mut out = json!({
                "type": "tool_use",
                "id": sanitize_tool_id(obj.get("id").and_then(|i| i.as_str()).unwrap_or("")),
                "name": obj.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                "input": obj.get("input").cloned().unwrap_or(json!({})),
            });
            if let Some(cc @ Value::Object(_)) = obj.get("cache_control") {
                out["cache_control"] = cc.clone();
            }
            Some(out)
        }
        Some("image") => {
            let src = obj.get("source")?;
            src.is_object().then(|| json!({"type": "image", "source": src.clone()}))
        }
        // Unknown block types on the input path — drop.
        _ => None,
    }
}

// ── Message conversion (anthropic_adapter.py:1936-2141) ─────────────────────

fn apply_assistant_cache_control_to_last_cacheable_block(blocks: &mut [Value], cache_control: Option<&Value>) {
    let Some(cc @ Value::Object(_)) = cache_control else {
        return;
    };
    for block in blocks.iter_mut().rev() {
        if matches!(block.get("type").and_then(|t| t.as_str()), Some("text") | Some("tool_use")) {
            if block.get("cache_control").is_none() {
                block["cache_control"] = cc.clone();
            }
            break;
        }
    }
}

/// Convert an assistant message (`_convert_assistant_message`,
/// anthropic_adapter.py:1948-2062): ordered-block replay fast path with
/// redacted tool-input re-sourcing, then preserved thinking blocks + content +
/// tool_use blocks + reasoning_content injection.
fn convert_assistant_message(m: &Map<String, Value>) -> Value {
    // Interleaved-thinking fast path: replay the captured ordered block list.
    if let Some(Value::Array(ordered)) = m.get("anthropic_content_blocks") {
        if !ordered.is_empty() {
            // Re-source each tool_use input from the stored tool_calls map:
            // block input was captured from the RAW response; the stored
            // tool_calls arguments are the canonical (redacted) copy.
            let mut redacted_input_by_id: std::collections::HashMap<String, Value> = Default::default();
            if let Some(Value::Array(tcs)) = m.get("tool_calls") {
                for tc in tcs {
                    let Some(tc) = tc.as_object() else { continue };
                    let fun = tc.get("function").and_then(|f| f.as_object());
                    let raw_args = fun
                        .and_then(|f| f.get("arguments"))
                        .cloned()
                        .unwrap_or(json!("{}"));
                    let parsed = match &raw_args {
                        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({})),
                        other => other.clone(),
                    };
                    let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    redacted_input_by_id.insert(sanitize_tool_id(id), parsed);
                }
            }
            let mut replayed: Vec<Value> = Vec::new();
            for b in ordered {
                let Some(mut clean) = sanitize_replay_block(b) else {
                    continue;
                };
                if clean.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let id = clean.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    if let Some(redacted) = redacted_input_by_id.get(&id) {
                        clean["input"] = redacted.clone();
                    }
                }
                replayed.push(clean);
            }
            if !replayed.is_empty() {
                apply_assistant_cache_control_to_last_cacheable_block(&mut replayed, m.get("cache_control"));
                return json!({"role": "assistant", "content": replayed});
            }
        }
    }

    // Preserved thinking blocks lead (anthropic_adapter.py:1828-1842, 2007).
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(Value::Array(details)) = m.get("reasoning_details") {
        for d in details {
            let Some(obj) = d.as_object() else { continue };
            let btype = obj
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if btype == "thinking" || btype == "redacted_thinking" {
                blocks.push(d.clone());
            }
        }
    }
    let content = m.get("content").cloned().unwrap_or(Value::Null);
    match &content {
        Value::Array(_) => {
            if let Value::Array(converted) = convert_content_to_anthropic(&content) {
                blocks.extend(converted);
            }
        }
        Value::String(s) if !s.is_empty() => blocks.push(json!({"type": "text", "text": s})),
        _ => {}
    }
    if let Some(Value::Array(tcs)) = m.get("tool_calls") {
        for tc in tcs {
            let Some(tc) = tc.as_object() else { continue };
            let fun = tc.get("function").and_then(|f| f.as_object());
            let raw_args = fun.and_then(|f| f.get("arguments")).cloned().unwrap_or(json!("{}"));
            let parsed = match &raw_args {
                Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({})),
                other => other.clone(),
            };
            blocks.push(json!({
                "type": "tool_use",
                "id": sanitize_tool_id(tc.get("id").and_then(|i| i.as_str()).unwrap_or("")),
                "name": fun.and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or(""),
                "input": parsed,
            }));
        }
    }
    apply_assistant_cache_control_to_last_cacheable_block(&mut blocks, m.get("cache_control"));
    // reasoning_content → leading unsigned thinking block, only when no
    // thinking blocks came from reasoning_details (anthropic_adapter.py:2033-2057).
    let already_has_thinking = blocks.iter().any(|b| {
        matches!(
            b.get("type").and_then(|t| t.as_str()),
            Some("thinking") | Some("redacted_thinking")
        )
    });
    if let Some(rc) = m.get("reasoning_content").and_then(|r| r.as_str()) {
        if !already_has_thinking {
            blocks.insert(0, json!({"type": "thinking", "thinking": rc}));
        }
    }
    // Anthropic rejects empty assistant content (2058-2062).
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": "(empty)"}));
    }
    json!({"role": "assistant", "content": blocks})
}

/// Convert a tool message into an Anthropic tool_result, merging consecutive
/// results into ONE user message (`_convert_tool_message_to_result`,
/// anthropic_adapter.py:2065-2124). Parallel tool calls therefore produce a
/// single user message containing all tool_results.
fn convert_tool_message_to_result(result: &mut Vec<Value>, m: &Map<String, Value>) {
    let content = m.get("content").cloned().unwrap_or(Value::Null);
    let mut multimodal_blocks: Option<Vec<Value>> = None;
    if content.is_array() {
        let converted = content_parts_to_anthropic_blocks(&content);
        if converted
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"))
        {
            multimodal_blocks = Some(converted);
        }
    }

    let result_content: Value = if let Some(blocks) = multimodal_blocks {
        Value::Array(blocks)
    } else if let Value::String(s) = &content {
        if s.is_empty() {
            json!("(no output)")
        } else {
            json!(s)
        }
    } else if content.is_null() {
        json!("(no output)")
    } else {
        json!(content.to_string())
    };

    let mut tool_result = json!({
        "type": "tool_result",
        "tool_use_id": sanitize_tool_id(m.get("tool_call_id").and_then(|i| i.as_str()).unwrap_or("")),
        "content": result_content,
    });
    if let Some(cc @ Value::Object(_)) = m.get("cache_control") {
        tool_result["cache_control"] = cc.clone();
    }

    // Merge consecutive tool results into one user message (2114-2124).
    let can_merge = result
        .last()
        .map(|last| {
            last.get("role").and_then(|r| r.as_str()) == Some("user")
                && last
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        !arr.is_empty()
                            && arr[0].get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    })
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    if can_merge {
        if let Some(arr) = result
            .last_mut()
            .and_then(|last| last.get_mut("content"))
            .and_then(|c| c.as_array_mut())
        {
            arr.push(tool_result);
        }
    } else {
        result.push(json!({"role": "user", "content": [tool_result]}));
    }
}

/// Validate and convert a user message (`_convert_user_message`,
/// anthropic_adapter.py:2127-2141). Empty content → "(empty message)".
fn convert_user_message(content: &Value) -> Value {
    if content.is_array() {
        let converted = convert_content_to_anthropic(content);
        let blocks = converted.as_array().cloned().unwrap_or_default();
        // Upstream check verbatim: empty list, or every *text* block blank
        // (vacuously true when there are no text blocks), → placeholder.
        let all_text_blank = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .all(|b| {
                b.get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .is_empty()
            });
        let final_blocks = if blocks.is_empty() || all_text_blank {
            vec![json!({"type": "text", "text": "(empty message)"})]
        } else {
            blocks
        };
        json!({"role": "user", "content": final_blocks})
    } else {
        let s = content.as_str().unwrap_or("");
        let text = if s.trim().is_empty() { "(empty message)" } else { s };
        json!({"role": "user", "content": text})
    }
}

// ── History shaping (anthropic_adapter.py:2144-2412) ────────────────────────

/// Strip tool_use blocks with no ADJACENT tool_result, and tool_results with
/// no surviving tool_use (`_strip_orphaned_tool_blocks`, 2144-2222).
fn strip_orphaned_tool_blocks(result: &mut [Value]) {
    // Pass 1: adjacency check per assistant message.
    for i in 0..result.len() {
        if result[i].get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(content) = result[i].get("content").and_then(|c| c.as_array()).cloned() else {
            continue;
        };
        let tool_use_ids: std::collections::HashSet<String> = content
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .filter_map(|b| b.get("id").and_then(|i| i.as_str()).map(str::to_string))
            .collect();
        if tool_use_ids.is_empty() {
            continue;
        }
        let mut adjacent_result_ids: std::collections::HashSet<String> = Default::default();
        if i + 1 < result.len() {
            let nxt = &result[i + 1];
            if nxt.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Some(blocks) = nxt.get("content").and_then(|c| c.as_array()) {
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            if let Some(id) = block.get("tool_use_id").and_then(|i| i.as_str()) {
                                adjacent_result_ids.insert(id.to_string());
                            }
                        }
                    }
                }
            }
        }
        let orphaned: std::collections::HashSet<&String> =
            tool_use_ids.iter().filter(|id| !adjacent_result_ids.contains(*id)).collect();
        if orphaned.is_empty() {
            continue;
        }
        let kept: Vec<Value> = content
            .iter()
            .filter(|b| {
                !(b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && b.get("id")
                        .and_then(|i| i.as_str())
                        .map(|id| orphaned.iter().any(|o| o.as_str() == id))
                        .unwrap_or(false))
            })
            .cloned()
            .collect();
        // Stripping mutated a turn carrying a signed thinking block → the
        // signature is dead; flag for _manage_thinking_signatures (2187-2199).
        if kept.len() != content.len()
            && content.iter().any(|b| {
                matches!(
                    b.get("type").and_then(|t| t.as_str()),
                    Some("thinking") | Some("redacted_thinking")
                )
            })
        {
            result[i]["_thinking_signature_invalidated"] = json!(true);
        }
        result[i]["content"] = if kept.is_empty() {
            json!([{"type": "text", "text": "(tool call removed)"}])
        } else {
            Value::Array(kept)
        };
    }

    // Pass 2: strip tool_results with no surviving tool_use anywhere.
    let surviving: std::collections::HashSet<String> = result
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .filter_map(|m| m.get("content").and_then(|c| c.as_array()))
        .flatten()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| b.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .collect();
    for m in result.iter_mut() {
        if m.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let Some(content) = m.get("content").and_then(|c| c.as_array()).cloned() else {
            continue;
        };
        let new_content: Vec<Value> = content
            .iter()
            .filter(|b| {
                b.get("type").and_then(|t| t.as_str()) != Some("tool_result")
                    || b.get("tool_use_id")
                        .and_then(|i| i.as_str())
                        .map(|id| surviving.contains(id))
                        .unwrap_or(false)
            })
            .cloned()
            .collect();
        if new_content.len() != content.len() {
            m["content"] = if new_content.is_empty() {
                json!([{"type": "text", "text": "(tool result removed)"}])
            } else {
                Value::Array(new_content)
            };
        }
    }
}

fn content_to_blocks(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        other => vec![json!({"type": "text", "text": other.to_string()})],
    }
}

/// Merge consecutive same-role messages to enforce Anthropic alternation
/// (`_merge_consecutive_roles`, anthropic_adapter.py:2225-2274).
fn merge_consecutive_roles(result: Vec<Value>) -> Vec<Value> {
    let mut fixed: Vec<Value> = Vec::new();
    for m in result {
        let same_role = fixed
            .last()
            .map(|prev| prev.get("role") == m.get("role"))
            .unwrap_or(false);
        if !same_role {
            fixed.push(m);
            continue;
        }
        let prev = fixed.last_mut().unwrap();
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
        let prev_content = prev.get("content").cloned().unwrap_or(Value::Null);
        let mut curr_content = m.get("content").cloned().unwrap_or(Value::Null);
        if role == "user" {
            match (&prev_content, &curr_content) {
                (Value::String(a), Value::String(b)) => {
                    prev["content"] = json!(format!("{a}\n{b}"));
                }
                (Value::Array(a), Value::Array(b)) => {
                    let mut merged = a.clone();
                    merged.extend(b.clone());
                    prev["content"] = Value::Array(merged);
                }
                _ => {
                    let mut merged = content_to_blocks(prev_content);
                    merged.extend(content_to_blocks(curr_content));
                    prev["content"] = Value::Array(merged);
                }
            }
        } else {
            // Consecutive assistant messages. Propagate the signature-
            // invalidation flag; drop thinking blocks from the SECOND message
            // (their signature was computed against a different turn boundary).
            if m.get("_thinking_signature_invalidated").and_then(|v| v.as_bool()) == Some(true) {
                prev["_thinking_signature_invalidated"] = json!(true);
            }
            if let Value::Array(blocks) = &curr_content {
                curr_content = Value::Array(
                    blocks
                        .iter()
                        .filter(|b| {
                            !matches!(
                                b.get("type").and_then(|t| t.as_str()),
                                Some("thinking") | Some("redacted_thinking")
                            )
                        })
                        .cloned()
                        .collect(),
                );
            }
            match (&prev_content, &curr_content) {
                (Value::Array(a), Value::Array(b)) => {
                    let mut merged = a.clone();
                    merged.extend(b.clone());
                    prev["content"] = Value::Array(merged);
                }
                (Value::String(a), Value::String(b)) => {
                    prev["content"] = json!(format!("{a}\n{b}"));
                }
                _ => {
                    let mut merged = content_to_blocks(prev_content);
                    merged.extend(content_to_blocks(curr_content));
                    prev["content"] = Value::Array(merged);
                }
            }
        }
    }
    fixed
}

fn is_third_party_anthropic_endpoint(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_lowercase();
    if normalized.is_empty() {
        return false; // no base_url = direct Anthropic API
    }
    !normalized.contains("anthropic.com")
}

fn is_kimi_family_endpoint(base_url: &str, model: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_lowercase();
    if normalized.starts_with("https://api.kimi.com/coding") {
        return true;
    }
    let host = joey_core::utils::base_url_hostname(base_url);
    for domain in ["api.kimi.com", "moonshot.ai", "moonshot.cn"] {
        if host == domain || host.ends_with(&format!(".{domain}")) {
            return true;
        }
    }
    model_name_is_kimi_family(model)
}

fn is_deepseek_anthropic_endpoint(base_url: &str) -> bool {
    // Pinned to the /anthropic path so the OpenAI-compatible api.deepseek.com
    // base URL is not misclassified (anthropic_adapter.py:522-539).
    let host = joey_core::utils::base_url_hostname(base_url);
    (host == "api.deepseek.com" || host.ends_with(".deepseek.com"))
        && base_url.to_lowercase().contains("/anthropic")
}

/// Strip or preserve thinking blocks based on endpoint type
/// (`_manage_thinking_signatures`, anthropic_adapter.py:2277-2377).
fn manage_thinking_signatures(result: &mut [Value], base_url: &str, model: &str) {
    let is_thinking = |b: &Value| {
        matches!(
            b.get("type").and_then(|t| t.as_str()),
            Some("thinking") | Some("redacted_thinking")
        )
    };
    let third_party = is_third_party_anthropic_endpoint(base_url);
    let last_assistant_idx = result
        .iter()
        .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"));

    #[allow(clippy::needless_range_loop)]
    for idx in 0..result.len() {
        if result[idx].get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(content) = result[idx].get("content").and_then(|c| c.as_array()).cloned() else {
            continue;
        };

        if is_kimi_family_endpoint(base_url, model) {
            // Kimi does not enforce signatures — replay as-is.
        } else if is_deepseek_anthropic_endpoint(base_url) {
            // DeepSeek: strip signed, preserve unsigned.
            let new_content: Vec<Value> = content
                .iter()
                .filter(|b| {
                    if !is_thinking(b) {
                        return true;
                    }
                    let signed = b.get("signature").map(truthy).unwrap_or(false)
                        || b.get("data").map(truthy).unwrap_or(false);
                    !signed
                })
                .cloned()
                .collect();
            result[idx]["content"] = if new_content.is_empty() {
                json!([{"type": "text", "text": "(empty)"}])
            } else {
                Value::Array(new_content)
            };
        } else if third_party || Some(idx) != last_assistant_idx {
            // Third-party: strip ALL thinking blocks (signatures are
            // Anthropic-proprietary). Direct Anthropic: strip from non-latest
            // assistant messages only.
            let stripped: Vec<Value> = content.iter().filter(|b| !is_thinking(b)).cloned().collect();
            result[idx]["content"] = if stripped.is_empty() {
                json!([{"type": "text", "text": "(thinking elided)"}])
            } else {
                Value::Array(stripped)
            };
        } else {
            // Latest assistant on direct Anthropic: keep signed, downgrade
            // unsigned to text; if the turn was structurally mutated the
            // signatures are dead — demote ALL thinking to text.
            let signature_dead = result[idx]
                .get("_thinking_signature_invalidated")
                .and_then(|v| v.as_bool())
                == Some(true);
            let mut new_content: Vec<Value> = Vec::new();
            for b in &content {
                if !is_thinking(b) {
                    new_content.push(b.clone());
                    continue;
                }
                if signature_dead {
                    if let Some(text) = b.get("thinking").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            new_content.push(json!({"type": "text", "text": text}));
                        }
                    }
                    continue;
                }
                if b.get("type").and_then(|t| t.as_str()) == Some("redacted_thinking") {
                    if b.get("data").map(truthy).unwrap_or(false) {
                        new_content.push(b.clone());
                    }
                } else if b.get("signature").map(truthy).unwrap_or(false) {
                    new_content.push(b.clone());
                } else if let Some(text) = b.get("thinking").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        new_content.push(json!({"type": "text", "text": text}));
                    }
                }
            }
            result[idx]["content"] = if new_content.is_empty() {
                json!([{"type": "text", "text": "(empty)"}])
            } else {
                Value::Array(new_content)
            };
        }

        // Strip cache_control from remaining thinking blocks — cache markers
        // interfere with signature validation (2370-2374).
        if let Some(blocks) = result[idx].get_mut("content").and_then(|c| c.as_array_mut()) {
            for b in blocks {
                if is_thinking(b) {
                    if let Some(obj) = b.as_object_mut() {
                        obj.remove("cache_control");
                    }
                }
            }
        }
        // Drop the internal bookkeeping flag (2376-2377).
        if let Some(obj) = result[idx].as_object_mut() {
            obj.remove("_thinking_signature_invalidated");
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
    }
}

/// Keep only the most recent 3 computer-use screenshots
/// (`_evict_old_screenshots`, anthropic_adapter.py:2380-2412).
fn evict_old_screenshots(result: &mut [Value]) {
    const MAX_KEEP_IMAGES: usize = 3;
    let mut image_count = 0usize;
    for msg in result.iter_mut().rev() {
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            let Some(inner) = block.get_mut("content").and_then(|c| c.as_array_mut()) else {
                continue;
            };
            let has_image = inner
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"));
            if !has_image {
                continue;
            }
            image_count += 1;
            if image_count > MAX_KEEP_IMAGES {
                for b in inner.iter_mut() {
                    if b.get("type").and_then(|t| t.as_str()) == Some("image") {
                        *b = json!({"type": "text", "text": "[screenshot removed to save context]"});
                    }
                }
            }
        }
    }
}

/// Convert OpenAI-format messages to Anthropic format
/// (`convert_messages_to_anthropic`, anthropic_adapter.py:2415-2476).
/// Returns `(system, messages)` — system is a string or block list.
pub(crate) fn convert_messages_to_anthropic(
    messages: &[Value],
    base_url: &str,
    model: &str,
) -> (Option<Value>, Vec<Value>) {
    let mut system: Option<Value> = None;
    let mut result: Vec<Value> = Vec::new();

    for m in messages {
        let Some(m) = m.as_object() else { continue };
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = m.get("content").cloned().unwrap_or(json!(""));

        match role {
            "system" => {
                if let Value::Array(parts) = &content {
                    // Preserve cache_control markers on content blocks (2444-2455).
                    let has_cache = parts.iter().any(|p| {
                        p.get("cache_control").map(truthy).unwrap_or(false)
                    });
                    if has_cache {
                        system = Some(Value::Array(
                            parts.iter().filter(|p| p.is_object()).cloned().collect(),
                        ));
                    } else {
                        let text: Vec<&str> = parts
                            .iter()
                            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect();
                        system = Some(json!(text.join("\n")));
                    }
                } else {
                    system = Some(content);
                }
            }
            "assistant" => result.push(convert_assistant_message(m)),
            "tool" => convert_tool_message_to_result(&mut result, m),
            _ => result.push(convert_user_message(&content)),
        }
    }

    strip_orphaned_tool_blocks(&mut result);
    let mut result = merge_consecutive_roles(result);
    manage_thinking_signatures(&mut result, base_url, model);
    evict_old_screenshots(&mut result);

    (system, result)
}

// ── Prompt caching (agent/prompt_caching.py, native layout) ─────────────────

/// Apply the upstream default `system_and_3` caching strategy: 4 ephemeral
/// cache_control breakpoints — system prompt + last 3 non-system messages
/// (prompt_caching.apply_anthropic_cache_control, native layout). Runs on the
/// OpenAI-shaped message list BEFORE conversion, matching upstream order; the
/// converter then relocates markers onto system blocks, tool_result blocks
/// (anthropic_adapter.py:2112-2113) and assistant content blocks. Default-on
/// for Claude models on the Anthropic wire (agent_init.py:637-644,
/// agent_runtime_helpers.anthropic_prompt_cache_policy). The 1h-TTL config
/// tier is not ported — markers are always `{"type": "ephemeral"}` (5m).
pub(crate) fn apply_anthropic_cache_control(messages: &mut [Value]) {
    let marker = json!({"type": "ephemeral"});
    let apply = |msg: &mut Value, marker: &Value| {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
        let content = msg.get("content").cloned();
        if role == "tool" {
            // Native layout: top-level marker; the adapter moves it inside
            // the tool_result block.
            msg["cache_control"] = marker.clone();
            return;
        }
        match content {
            None | Some(Value::Null) => {
                msg["cache_control"] = marker.clone();
            }
            Some(Value::String(s)) => {
                if s.is_empty() {
                    msg["cache_control"] = marker.clone();
                } else {
                    msg["content"] =
                        json!([{"type": "text", "text": s, "cache_control": marker.clone()}]);
                }
            }
            Some(Value::Array(mut parts)) => {
                if let Some(last) = parts.last_mut() {
                    if last.is_object() {
                        last["cache_control"] = marker.clone();
                    }
                }
                msg["content"] = Value::Array(parts);
            }
            _ => {}
        }
    };

    let mut breakpoints_used = 0;
    if !messages.is_empty()
        && messages[0].get("role").and_then(|r| r.as_str()) == Some("system")
    {
        apply(&mut messages[0], &marker);
        breakpoints_used += 1;
    }
    let remaining = 4 - breakpoints_used;
    let non_sys: Vec<usize> = (0..messages.len())
        .filter(|&i| messages[i].get("role").and_then(|r| r.as_str()) != Some("system"))
        .collect();
    let start = non_sys.len().saturating_sub(remaining);
    for &idx in &non_sys[start..] {
        apply(&mut messages[idx], &marker);
    }
}

// ── Request body (build_anthropic_kwargs, anthropic_adapter.py:2479-2714) ───

/// Serialize a [`crate::types::Message`] into the OpenAI-dict shape the
/// conversion pipeline consumes (upstream messages are OpenAI dicts).
fn message_to_openai_value(m: &crate::types::Message) -> Value {
    let mut obj = Map::new();
    obj.insert("role".into(), json!(m.role));
    if let Some(parts) = &m.content_parts {
        obj.insert("content".into(), serde_json::to_value(parts).unwrap_or(Value::Null));
    } else {
        obj.insert("content".into(), json!(m.content.clone().unwrap_or_default()));
    }
    if !m.tool_calls.is_empty() {
        obj.insert("tool_calls".into(), serde_json::to_value(&m.tool_calls).unwrap_or(Value::Null));
    }
    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), json!(id));
    }
    if let Some(rc) = &m.reasoning {
        obj.insert("reasoning_content".into(), json!(rc));
    }
    if let Some(rd) = &m.reasoning_details {
        obj.insert("reasoning_details".into(), rd.clone());
    }
    if let Some(blocks) = &m.anthropic_content_blocks {
        obj.insert("anthropic_content_blocks".into(), blocks.clone());
    }
    Value::Object(obj)
}

/// Build the Anthropic Messages request body (port of
/// `build_anthropic_kwargs`, minus the unreplicated OAuth identity layer).
/// The `stream` flag is added by the client.
pub(crate) fn build_anthropic_body(req: &ProviderRequest, base_url: &str) -> Value {
    // Assemble the OpenAI-shaped list: leading system + history.
    let mut messages_json: Vec<Value> = Vec::new();
    if let Some(sys) = &req.system {
        if !sys.is_empty() {
            messages_json.push(json!({"role": "system", "content": sys}));
        }
    }
    for m in &req.messages {
        messages_json.push(message_to_openai_value(m));
    }

    // Prompt caching: default-on for Claude on the Anthropic wire
    // (anthropic_prompt_cache_policy: native anthropic → (True, True);
    // third-party anthropic wire + Claude → (True, True)).
    if is_claude_model(&req.model) {
        apply_anthropic_cache_control(&mut messages_json);
    }

    let model = normalize_model_name(&req.model);
    let (system, anthropic_messages) = convert_messages_to_anthropic(&messages_json, base_url, &model);
    let anthropic_tools = convert_tools_to_anthropic(&req.tools);

    let effective_max_tokens = resolve_anthropic_max_tokens(req.max_tokens, &model) as u64;

    let mut body = json!({
        "model": model,
        "messages": anthropic_messages,
        "max_tokens": effective_max_tokens,
    });
    let obj = body.as_object_mut().unwrap();

    if let Some(system) = system {
        if truthy(&system) {
            obj.insert("system".into(), system);
        }
    }

    if !anthropic_tools.is_empty() {
        obj.insert("tools".into(), json!(anthropic_tools));
        // OpenAI tool_choice "auto"/None → Anthropic {"type": "auto"}
        // (anthropic_adapter.py:2629-2631).
        obj.insert("tool_choice".into(), json!({"type": "auto"}));
    }

    if let Some(t) = req.temperature {
        obj.insert("temperature".into(), json!(t));
    }

    // Extended thinking (anthropic_adapter.py:2659-2680). Haiku models get
    // no thinking at all; adaptive-capable models get adaptive + effort;
    // legacy models get manual budget + temperature=1 + max_tokens bump.
    if let Some(ReasoningEffort::Level(level)) = &req.reasoning {
        if !model.to_lowercase().contains("haiku") {
            let effort = level.trim().to_lowercase();
            let budget = thinking_budget(&effort);
            if supports_adaptive_thinking(&model) {
                obj.insert(
                    "thinking".into(),
                    json!({"type": "adaptive", "display": "summarized"}),
                );
                let mut eff = adaptive_effort(&effort);
                // Downgrade xhigh→max on 4.6 (anthropic_adapter.py:2669-2672).
                if eff == "xhigh" && !supports_xhigh_effort(&model) {
                    eff = "max";
                }
                obj.insert("output_config".into(), json!({"effort": eff}));
            } else {
                obj.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": budget}));
                // Thinking requires temperature=1 on older models.
                obj.insert("temperature".into(), json!(1));
                obj.insert(
                    "max_tokens".into(),
                    json!(std::cmp::max(effective_max_tokens, budget + 4096)),
                );
            }
        }
    }

    // Strip sampling params on 4.7+ (anthropic_adapter.py:2687-2689).
    if forbids_sampling_params(&model) {
        for key in ["temperature", "top_p", "top_k"] {
            obj.remove(key);
        }
    }

    body
}

// ── Response normalization (transports/anthropic.py:80-192) ─────────────────

/// Parse a non-streaming Anthropic Messages response.
pub(crate) fn parse_anthropic_response(v: &Value) -> Result<NormalizedResponse, crate::error::ProviderError> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut reasoning_details: Vec<Value> = Vec::new();
    let mut ordered_blocks: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            // Sanitize at capture so output-only fields never persist and
            // leak back as request input (transports/anthropic.py:110-119).
            let clean = sanitize_replay_block(b);
            if let Some(clean) = &clean {
                ordered_blocks.push(clean.clone());
            }
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
                Some("thinking") => {
                    if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                        reasoning_parts.push(t.to_string());
                    }
                    reasoning_details.push(clean.unwrap_or_else(|| b.clone()));
                }
                Some("redacted_thinking") => {
                    if let Some(clean) = clean {
                        reasoning_details.push(clean);
                    }
                }
                Some("tool_use") => {
                    let id = b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let input = b.get("input").cloned().unwrap_or(json!({}));
                    tool_calls.push(ToolCall::new(id, name, input.to_string()));
                }
                _ => {}
            }
        }
    }

    let finish = v
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .map(FinishReason::from_wire)
        .unwrap_or(FinishReason::Stop);
    let mut usage = Usage::default();
    if let Some(u) = v.get("usage") {
        merge_anthropic_usage(&mut usage, u);
    }
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);

    Ok(finalize_anthropic_response(
        text_parts,
        reasoning_parts,
        reasoning_details,
        ordered_blocks,
        tool_calls,
        finish,
        usage,
        model,
    ))
}

/// Shared tail for stream + non-stream Anthropic normalization: joins text
/// with "\n" and reasoning with "\n\n" (transports/anthropic.py:185-190) and
/// gates the ordered-blocks channel on signed-thinking + tool_use
/// (transports/anthropic.py:167-183).
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_anthropic_response(
    text_parts: Vec<String>,
    reasoning_parts: Vec<String>,
    reasoning_details: Vec<Value>,
    ordered_blocks: Vec<Value>,
    tool_calls: Vec<ToolCall>,
    finish: FinishReason,
    usage: Usage,
    model: Option<String>,
) -> NormalizedResponse {
    let has_signed_thinking = ordered_blocks.iter().any(|b| {
        matches!(
            b.get("type").and_then(|t| t.as_str()),
            Some("thinking") | Some("redacted_thinking")
        ) && (b.get("signature").map(truthy).unwrap_or(false)
            || b.get("data").map(truthy).unwrap_or(false))
    });
    let has_tool_use = ordered_blocks
        .iter()
        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));

    let mut finish = finish;
    if !tool_calls.is_empty() && finish == FinishReason::Stop {
        finish = FinishReason::ToolCalls;
    }

    NormalizedResponse {
        content: text_parts.join("\n"),
        tool_calls,
        finish_reason: finish,
        reasoning: (!reasoning_parts.is_empty()).then(|| reasoning_parts.join("\n\n")),
        usage,
        model,
        reasoning_details: (!reasoning_details.is_empty()).then(|| Value::Array(reasoning_details)),
        anthropic_content_blocks: (has_signed_thinking && has_tool_use)
            .then(|| Value::Array(ordered_blocks)),
    }
}

pub(crate) fn merge_anthropic_usage(usage: &mut Usage, u: &Value) {
    let get = |k: &str| u.get(k).and_then(|v| v.as_u64());
    if let Some(i) = get("input_tokens") {
        usage.prompt_tokens = i;
    }
    if let Some(o) = get("output_tokens") {
        usage.completion_tokens = o;
    }
    if let Some(c) = get("cache_read_input_tokens") {
        usage.cache_read_tokens = c;
    }
    if let Some(c) = get("cache_creation_input_tokens") {
        usage.cache_write_tokens = c;
    }
    usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolCall};

    const ANT: &str = "https://api.anthropic.com";

    fn req(model: &str, messages: Vec<Message>) -> ProviderRequest {
        ProviderRequest::new(model, messages)
    }

    fn body_obj(req: &ProviderRequest) -> Map<String, Value> {
        build_anthropic_body(req, ANT).as_object().unwrap().clone()
    }

    #[test]
    fn normalize_model_name_cases() {
        // H2: strip anthropic/, dots→hyphens for claude-*.
        assert_eq!(normalize_model_name("anthropic/claude-opus-4.6"), "claude-opus-4-6");
        assert_eq!(normalize_model_name("claude-sonnet-4.6"), "claude-sonnet-4-6");
        // Bedrock IDs preserved.
        assert_eq!(normalize_model_name("anthropic.claude-opus-4-7"), "anthropic.claude-opus-4-7");
        assert_eq!(
            normalize_model_name("us.anthropic.claude-sonnet-4-5-v1:0"),
            "us.anthropic.claude-sonnet-4-5-v1:0"
        );
        // Non-Anthropic models untouched.
        assert_eq!(normalize_model_name("gpt-5.4"), "gpt-5.4");
        assert_eq!(normalize_model_name("gemini-2.5-pro"), "gemini-2.5-pro");
    }

    #[test]
    fn adaptive_thinking_4_6_downgrades_xhigh() {
        let mut r = req("anthropic/claude-opus-4.6", vec![Message::user("hi")]);
        r.reasoning = Some(ReasoningEffort::Level("xhigh".into()));
        let b = body_obj(&r);
        assert_eq!(b["model"], json!("claude-opus-4-6"));
        assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        // xhigh downgraded to max on 4.6.
        assert_eq!(b["output_config"], json!({"effort": "max"}));
        assert!(b.get("budget_tokens").is_none());
    }

    #[test]
    fn adaptive_thinking_4_7_keeps_xhigh_strips_sampling() {
        let mut r = req("claude-opus-4-7", vec![Message::user("hi")]);
        r.reasoning = Some(ReasoningEffort::Level("xhigh".into()));
        r.temperature = Some(0.7);
        let b = body_obj(&r);
        assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(b["output_config"], json!({"effort": "xhigh"}));
        // 4.7 forbids sampling params — temperature stripped.
        assert!(b.get("temperature").is_none());
    }

    #[test]
    fn legacy_thinking_uses_budget_and_temperature_1() {
        let mut r = req("claude-3-5-sonnet", vec![Message::user("hi")]);
        r.reasoning = Some(ReasoningEffort::Level("high".into()));
        let b = body_obj(&r);
        assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": 16000}));
        assert_eq!(b["temperature"], json!(1));
        // max_tokens = max(effective 8192, budget 16000 + 4096) = 20096.
        assert_eq!(b["max_tokens"], json!(20096));
    }

    #[test]
    fn haiku_gets_no_thinking() {
        let mut r = req("claude-haiku-4-5", vec![Message::user("hi")]);
        r.reasoning = Some(ReasoningEffort::Level("high".into()));
        let b = body_obj(&r);
        assert!(b.get("thinking").is_none());
        assert!(b.get("output_config").is_none());
    }

    #[test]
    fn thinking_budget_unknown_effort_defaults_8000() {
        assert_eq!(thinking_budget("weird"), 8000);
        assert_eq!(thinking_budget("low"), 4000);
    }

    #[test]
    fn max_tokens_resolution_default_ceiling() {
        // Caller unset → per-model output ceiling (opus 4.6 = 128000).
        let r = req("claude-opus-4.6", vec![Message::user("hi")]);
        assert_eq!(body_obj(&r)["max_tokens"], json!(128_000));
        // Caller set wins.
        let mut r = req("claude-opus-4.6", vec![Message::user("hi")]);
        r.max_tokens = Some(4096);
        assert_eq!(body_obj(&r)["max_tokens"], json!(4096));
        // Unknown model → default 128000.
        let r = req("claude-something-new", vec![Message::user("hi")]);
        assert_eq!(body_obj(&r)["max_tokens"], json!(128_000));
    }

    #[test]
    fn tool_choice_auto_when_tools_present() {
        // H9: Anthropic wire sends {"type":"auto"} when tools present.
        let mut r = req("claude-opus-4.6", vec![Message::user("hi")]);
        r.tools = vec![ToolSchema::new("t", "d", json!({"type": "object", "properties": {}}))];
        let b = body_obj(&r);
        assert_eq!(b["tool_choice"], json!({"type": "auto"}));
        // No tools → no tool_choice.
        let r = req("claude-opus-4.6", vec![Message::user("hi")]);
        assert!(body_obj(&r).get("tool_choice").is_none());
    }

    #[test]
    fn parallel_tool_results_merge_into_one_user_message() {
        // H5: parallel tool calls → a single user message with all tool_results.
        let assistant = Message::assistant_with_tools(
            Some("calling".into()),
            vec![
                ToolCall::new("call_a", "read_file", r#"{"path":"a"}"#),
                ToolCall::new("call_b", "read_file", r#"{"path":"b"}"#),
            ],
        );
        let messages = vec![
            Message::user("do it"),
            assistant,
            Message::tool_result("call_a", "read_file", "result A"),
            Message::tool_result("call_b", "read_file", "result B"),
        ];
        let r = req("claude-opus-4.6", messages);
        let b = body_obj(&r);
        let msgs = b["messages"].as_array().unwrap();
        // user, assistant(tool_use), user(2 tool_results).
        let tool_result_msgs: Vec<&Value> = msgs
            .iter()
            .filter(|m| {
                m["role"] == json!("user")
                    && m["content"]
                        .as_array()
                        .map(|a| a.iter().any(|b| b["type"] == json!("tool_result")))
                        .unwrap_or(false)
            })
            .collect();
        assert_eq!(tool_result_msgs.len(), 1, "tool_results must merge into ONE user message");
        let blocks = tool_result_msgs[0]["content"].as_array().unwrap();
        let trs: Vec<&Value> = blocks.iter().filter(|b| b["type"] == json!("tool_result")).collect();
        assert_eq!(trs.len(), 2, "both tool_results present in the merged message");
        assert_eq!(trs[0]["tool_use_id"], json!("call_a"));
        assert_eq!(trs[1]["tool_use_id"], json!("call_b"));
    }

    #[test]
    fn empty_tool_result_placeholder() {
        let messages = vec![
            Message::assistant_with_tools(None, vec![ToolCall::new("c1", "noop", "{}")]),
            Message::tool_result("c1", "noop", ""),
        ];
        let r = req("claude-opus-4.6", messages);
        let b = body_obj(&r);
        let msgs = b["messages"].as_array().unwrap();
        let tr = msgs
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .find(|b| b["type"] == json!("tool_result"))
            .unwrap();
        assert_eq!(tr["content"], json!("(no output)"));
    }

    #[test]
    fn thinking_replay_block_ordering() {
        // H4: a stored assistant turn with interleaved signed thinking + tool_use
        // replays the blocks in captured order, re-sourcing tool input from
        // the redacted tool_calls.
        let ordered = json!([
            {"type": "thinking", "thinking": "step 1", "signature": "sig1"},
            {"type": "tool_use", "id": "call_x", "name": "read_file", "input": {"path": "SECRET"}},
            {"type": "thinking", "thinking": "step 2", "signature": "sig2"},
            {"type": "text", "text": "done"},
        ]);
        let mut assistant = Message::assistant_with_tools(
            None,
            vec![ToolCall::new("call_x", "read_file", r#"{"path":"redacted"}"#)],
        );
        assistant.anthropic_content_blocks = Some(ordered);
        let messages = vec![
            Message::user("q"),
            assistant,
            Message::tool_result("call_x", "read_file", "ok"),
        ];
        let r = req("claude-opus-4.6", messages);
        let b = body_obj(&r);
        let msgs = b["messages"].as_array().unwrap();
        let asst = msgs.iter().find(|m| m["role"] == json!("assistant")).unwrap();
        let blocks = asst["content"].as_array().unwrap();
        // Order preserved: thinking, tool_use, thinking, text.
        assert_eq!(blocks[0]["type"], json!("thinking"));
        assert_eq!(blocks[0]["signature"], json!("sig1"));
        assert_eq!(blocks[1]["type"], json!("tool_use"));
        // Tool input re-sourced from the redacted tool_calls arguments.
        assert_eq!(blocks[1]["input"], json!({"path": "redacted"}));
        assert_eq!(blocks[2]["type"], json!("thinking"));
        assert_eq!(blocks[3]["type"], json!("text"));
    }

    #[test]
    fn reasoning_details_replay_as_leading_thinking() {
        // Preserved thinking blocks lead the assistant content on replay.
        let mut assistant = Message::assistant("here is the answer");
        assistant.reasoning_details = Some(json!([
            {"type": "thinking", "thinking": "prior reasoning", "signature": "s"}
        ]));
        let r = req("claude-opus-4.6", vec![Message::user("q"), assistant]);
        let b = body_obj(&r);
        let msgs = b["messages"].as_array().unwrap();
        let asst = msgs.iter().find(|m| m["role"] == json!("assistant")).unwrap();
        let blocks = asst["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], json!("thinking"));
        assert_eq!(blocks[0]["thinking"], json!("prior reasoning"));
    }

    #[test]
    fn third_party_strips_thinking_signatures() {
        // On a third-party Anthropic endpoint, ALL thinking blocks are stripped.
        let mut assistant = Message::assistant("answer");
        assistant.reasoning_details = Some(json!([
            {"type": "thinking", "thinking": "secret reasoning", "signature": "s"}
        ]));
        let r = req("claude-opus-4.6", vec![Message::user("q"), assistant]);
        let body = build_anthropic_body(&r, "https://my-proxy.example.com");
        let msgs = body["messages"].as_array().unwrap();
        let asst = msgs.iter().find(|m| m["role"] == json!("assistant")).unwrap();
        let has_thinking = asst["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == json!("thinking"));
        assert!(!has_thinking, "third-party endpoints cannot validate signatures");
    }

    #[test]
    fn tool_schema_nullable_union_stripped() {
        // M14: nullable anyOf collapses to the non-null branch.
        let schema = json!({
            "type": "object",
            "properties": {
                "x": {"anyOf": [{"type": "string"}, {"type": "null"}]}
            }
        });
        let r = {
            let mut r = req("claude-opus-4.6", vec![Message::user("hi")]);
            r.tools = vec![ToolSchema::new("t", "d", schema)];
            r
        };
        let b = body_obj(&r);
        let tool = &b["tools"][0];
        assert_eq!(tool["input_schema"]["properties"]["x"], json!({"type": "string"}));
    }

    #[test]
    fn tool_schema_top_level_union_removed_and_dedup() {
        let schema = json!({"oneOf": [{"type": "object"}, {"type": "string"}]});
        let mut r = req("claude-opus-4.6", vec![Message::user("hi")]);
        r.tools = vec![
            ToolSchema::new("dup", "d", schema),
            ToolSchema::new("dup", "d2", json!({"type": "object", "properties": {}})),
        ];
        let b = body_obj(&r);
        let tools = b["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "duplicate tool names deduped");
        // Top-level oneOf removed, floored to object.
        assert!(tools[0]["input_schema"].get("oneOf").is_none());
        assert_eq!(tools[0]["input_schema"]["type"], json!("object"));
    }

    #[test]
    fn tool_id_sanitized() {
        assert_eq!(sanitize_tool_id("call/weird id!"), "call_weird_id_");
        assert_eq!(sanitize_tool_id(""), "tool_0");
        let messages = vec![
            Message::assistant_with_tools(None, vec![ToolCall::new("bad id!", "t", "{}")]),
            Message::tool_result("bad id!", "t", "ok"),
        ];
        let r = req("claude-opus-4.6", messages);
        let b = body_obj(&r);
        let msgs = b["messages"].as_array().unwrap();
        let tu = msgs
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .find(|b| b["type"] == json!("tool_use"))
            .unwrap();
        assert_eq!(tu["id"], json!("bad_id_"));
    }

    #[test]
    fn beta_headers() {
        // M11: native requests get the two common betas, not OAuth betas.
        let h = anthropic_beta_header(ANT).unwrap();
        assert!(h.contains("interleaved-thinking-2025-05-14"));
        assert!(h.contains("fine-grained-tool-streaming-2025-05-14"));
        assert!(!h.contains("oauth"), "OAuth-only betas are not sent (policy)");
        assert!(!h.contains("context-1m"), "1M beta omitted on native Anthropic");
        // Azure endpoints add the 1M beta.
        let h = anthropic_beta_header("https://foo.services.ai.azure.com/anthropic").unwrap();
        assert!(h.contains("context-1m-2025-08-07"));
        // MiniMax strips fine-grained streaming.
        let h = anthropic_beta_header("https://api.minimax.io/anthropic").unwrap();
        assert!(!h.contains("fine-grained-tool-streaming"));
    }

    #[test]
    fn oauth_token_detection() {
        assert!(!is_oauth_token("sk-ant-api03-xyz"), "Console keys use x-api-key");
        assert!(is_oauth_token("sk-ant-oat01-xyz"));
        assert!(is_oauth_token("cc-abc123"));
        assert!(is_oauth_token("eyJhbGciOi..."));
        assert!(!is_oauth_token("glm-key-xyz"), "non-Anthropic keys are not OAuth");
        assert!(!is_oauth_token(""));
    }

    #[test]
    fn prompt_caching_marks_system_and_recent() {
        // M15: system prompt gets a cache breakpoint by default for Claude.
        let r = req("claude-opus-4.6", vec![Message::user("hi")]).with_system(Some("SYS".into()));
        let b = body_obj(&r);
        // System becomes a block list carrying cache_control.
        let sys = &b["system"];
        assert!(sys.is_array(), "system carries cache_control block list");
        let has_cc = sys.as_array().unwrap().iter().any(|blk| blk.get("cache_control").is_some());
        assert!(has_cc);
    }

    #[test]
    fn image_media_type_default_jpeg() {
        // L3: data URL without image/* MIME → default image/jpeg.
        let src = image_source_from_openai_url("data:;base64,QUJD");
        assert_eq!(src["media_type"], json!("image/jpeg"));
        let src = image_source_from_openai_url("data:image/png;base64,QUJD");
        assert_eq!(src["media_type"], json!("image/png"));
        // Non-image MIME rejected → falls back to jpeg default.
        let src = image_source_from_openai_url("data:text/plain;base64,QUJD");
        assert_eq!(src["media_type"], json!("image/jpeg"));
    }

    #[test]
    fn response_finish_reason_mapping() {
        // M10: refusal → ContentFilter, model_context_window_exceeded → Length.
        let resp = json!({
            "content": [{"type": "text", "text": "no"}],
            "stop_reason": "refusal",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let n = parse_anthropic_response(&resp).unwrap();
        assert_eq!(n.finish_reason, FinishReason::ContentFilter);
        let resp = json!({"content": [], "stop_reason": "model_context_window_exceeded"});
        let n = parse_anthropic_response(&resp).unwrap();
        assert_eq!(n.finish_reason, FinishReason::Length);
    }

    #[test]
    fn response_captures_signed_thinking_ordered_blocks() {
        // Ordered-blocks channel populated only for signed-thinking + tool_use.
        let resp = json!({
            "content": [
                {"type": "thinking", "thinking": "t", "signature": "sig"},
                {"type": "tool_use", "id": "c1", "name": "f", "input": {"a": 1}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 5}
        });
        let n = parse_anthropic_response(&resp).unwrap();
        assert!(n.anthropic_content_blocks.is_some());
        assert_eq!(n.finish_reason, FinishReason::ToolCalls);
        assert_eq!(n.tool_calls.len(), 1);
        // reasoning_details captured.
        assert!(n.reasoning_details.is_some());
        // A plain text response gets no ordered-blocks channel.
        let resp = json!({"content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn"});
        let n = parse_anthropic_response(&resp).unwrap();
        assert!(n.anthropic_content_blocks.is_none());
    }
}
