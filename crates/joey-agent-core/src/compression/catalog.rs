//! Model context-length catalog + lookup and provider-error context parsing
//! (port of the offline core of `agent/model_metadata.py`).
//!
//! The port resolves context lengths from: the explicit config override
//! (`model.context_length`), then the hardcoded family catalog
//! (longest-key-first substring match), then the 256K default. Upstream's
//! live probes (OpenRouter catalog, models.dev, local-server /props,
//! Anthropic /v1/models, Bedrock, Copilot, Codex OAuth, endpoint caches)
//! are deliberately not ported — they are network/provider machinery, and
//! the overflow handler's provider-error probe (`update_model` from
//! [`get_context_length_from_provider_error`]) covers the correction path.

use once_cell::sync::Lazy;
use regex::Regex;

/// Descending tiers for context length probing (model_metadata.py:179-186).
pub const CONTEXT_PROBE_TIERS: &[i64] = &[256_000, 128_000, 64_000, 32_000, 16_000, 8_000];

/// Default context length when no detection method succeeds (tier 0).
pub const DEFAULT_FALLBACK_CONTEXT: i64 = CONTEXT_PROBE_TIERS[0];

/// Minimum context length required to run the agent (model_metadata.py:194).
pub const MINIMUM_CONTEXT_LENGTH: i64 = 64_000;

/// Provider names that can appear as a "provider:" prefix before a model ID
/// (model_metadata.py `_PROVIDER_PREFIXES`).
const PROVIDER_PREFIXES: &[&str] = &[
    "openrouter", "nous", "openai-codex", "copilot", "copilot-acp",
    "gemini", "ollama-cloud", "zai", "kimi-coding", "kimi-coding-cn", "stepfun", "minimax",
    "minimax-oauth", "minimax-cn", "anthropic", "deepseek", "deepinfra",
    "opencode-zen", "opencode-go", "kilocode", "alibaba", "novita",
    "qwen-oauth",
    "xiaomi",
    "arcee",
    "gmi",
    "tencent-tokenhub",
    "custom", "local",
    // Common aliases
    "google", "google-gemini", "google-ai-studio",
    "glm", "z-ai", "z.ai", "zhipu", "github", "github-copilot",
    "github-models", "kimi", "moonshot", "kimi-cn", "moonshot-cn", "claude", "deep-seek",
    "deep-infra",
    "ollama",
    "opencode", "zen", "go", "kilo", "dashscope", "aliyun", "qwen",
    "mimo", "xiaomi-mimo",
    "tencent", "tokenhub", "tencent-cloud", "tencentmaas",
    "arcee-ai", "arceeai",
    "gmi-cloud", "gmicloud",
    "xai", "x-ai", "x.ai", "grok",
    "nvidia", "nim", "nvidia-nim", "nemotron",
    "qwen-portal", "novita-ai", "novitaai",
];

static OLLAMA_TAG_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(\d+\.?\d*b|latest|stable|q\d|fp?\d|instruct|chat|coder|vision|text)").unwrap()
});

/// Strip a recognised provider prefix from a model string
/// (`_strip_provider_prefix`). Ollama-style `model:tag` colons are preserved.
pub fn strip_provider_prefix(model: &str) -> &str {
    if !model.contains(':') || model.starts_with("http") {
        return model;
    }
    let (prefix, suffix) = match model.split_once(':') {
        Some(p) => p,
        None => return model,
    };
    let prefix_lower = prefix.trim().to_lowercase();
    if PROVIDER_PREFIXES.contains(&prefix_lower.as_str()) {
        // Don't strip if the suffix looks like an Ollama tag ("7b", "latest").
        if OLLAMA_TAG_PATTERN.is_match(suffix.trim()) {
            return model;
        }
        return suffix;
    }
    model
}

/// Thin fallback defaults — broad model family patterns, entries verbatim
/// from `DEFAULT_CONTEXT_LENGTHS` (model_metadata.py:209-366). Matched
/// longest-key-first as a substring of the lowercased model id.
pub const DEFAULT_CONTEXT_LENGTHS: &[(&str, i64)] = &[
    // Anthropic Claude 4.6 (1M context) — bare IDs only.
    ("claude-fable-5", 1_000_000),
    ("claude-fable", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-4.8", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4.7", 1_000_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-opus-4.6", 1_000_000),
    ("claude-sonnet-4.6", 1_000_000),
    // Catch-all for older Claude models
    ("claude", 200_000),
    // OpenAI — GPT-5 family
    ("gpt-5.6-luna", 1_050_000),
    ("gpt-5.6-terra", 1_050_000),
    ("gpt-5.6-sol", 1_050_000),
    ("gpt-5.5", 1_050_000),
    ("gpt-5.4-nano", 400_000),
    ("gpt-5.4-mini", 400_000),
    ("gpt-5.4", 1_050_000),
    ("gpt-5.3-codex-spark", 128_000),
    ("gpt-5.1-chat", 128_000),
    ("gpt-5", 400_000),
    ("gpt-4.1", 1_047_576),
    ("gpt-4", 128_000),
    // Google
    ("gemini", 1_048_576),
    ("gemma-4", 256_000),
    ("gemma4", 256_000),
    ("gemma-4-31b", 256_000),
    ("gemma-3", 131_072),
    ("gemma", 8_192),
    // DeepSeek
    ("deepseek-v4-pro", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
    ("deepseek-chat", 1_000_000),
    ("deepseek-reasoner", 1_000_000),
    ("deepseek", 128_000),
    // Meta
    ("llama", 131_072),
    // Qwen
    ("qwen3.6-plus", 1_048_576),
    ("qwen3.7-plus", 1_048_576),
    ("qwen3-coder-plus", 1_000_000),
    ("qwen3-coder", 262_144),
    ("qwen3-max", 262_144),
    ("qwen", 131_072),
    // MiniMax
    ("minimax-m3", 1_000_000),
    ("minimax", 204_800),
    // GLM
    ("glm-5.2", 1_048_576),
    ("glm", 202_752),
    // xAI Grok
    ("grok-composer", 200_000),
    ("grok-build-latest", 500_000),
    ("grok-build", 256_000),
    ("grok-code-fast", 256_000),
    ("grok-2-vision", 8_192),
    ("grok-4-fast", 2_000_000),
    ("grok-4.20", 2_000_000),
    ("grok-4.5", 500_000),
    ("grok-4.3", 1_000_000),
    ("grok-4", 256_000),
    ("grok-3", 131_072),
    ("grok-2", 131_072),
    ("grok", 131_072),
    // Kimi
    ("kimi-k3", 1_048_576),
    ("kimi", 262_144),
    // Upstage Solar
    ("solar-open2", 262_144),
    ("solar-pro3", 131_072),
    ("solar-pro2", 65_536),
    ("solar-mini", 32_768),
    // Tencent Hunyuan
    ("hy3-preview", 262_144),
    ("hy3", 262_144),
    // Nemotron
    ("nemotron", 131_072),
    // Arcee
    ("trinity", 262_144),
    // OpenRouter
    ("elephant", 262_144),
    // Hugging Face Inference Providers — model IDs use org/name format.
    // Keys are kept VERBATIM (mixed case): the lookup lowercases the model
    // id but not the key (upstream `default_model in model_lower`), so the
    // capitalized entries never substring-match — a latent upstream no-op
    // the port reproduces exactly rather than "fixing".
    ("Qwen/Qwen3.5-397B-A17B", 131_072),
    ("Qwen/Qwen3.5-35B-A3B", 131_072),
    ("deepseek-ai/DeepSeek-V3.2", 65_536),
    ("moonshotai/Kimi-K2.5", 262_144),
    ("moonshotai/Kimi-K2.6", 262_144),
    ("moonshotai/Kimi-K2-Thinking", 262_144),
    ("MiniMaxAI/MiniMax-M2.5", 204_800),
    ("XiaomiMiMo/MiMo-V2-Flash", 262_144),
    ("mimo-v2-pro", 1_048_576),
    ("mimo-v2.5-pro", 1_048_576),
    ("mimo-v2.5", 1_048_576),
    ("mimo-v2-omni", 262_144),
    ("mimo-v2-flash", 262_144),
    ("zai-org/GLM-5", 202_752),
];

/// Get the context length for a model (the offline subset of
/// `get_model_context_length`):
///
/// 0. Explicit config override (`model.context_length`) — user knows best.
/// 8. Hardcoded defaults (fuzzy match — longest key first for specificity;
///    only `default_model in model`, never the reverse).
/// 9. Default fallback — 256K.
pub fn get_model_context_length(model: &str, config_context_length: Option<i64>) -> i64 {
    if let Some(ctx) = config_context_length {
        if ctx > 0 {
            return ctx;
        }
    }
    let model = strip_provider_prefix(model);
    let model_lower = model.to_lowercase();
    let mut entries: Vec<&(&str, i64)> = DEFAULT_CONTEXT_LENGTHS.iter().collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    for (default_model, length) in entries {
        if model_lower.contains(default_model) {
            return *length;
        }
    }
    DEFAULT_FALLBACK_CONTEXT
}

/// Return the next lower probe tier, or None if already at minimum
/// (`get_next_probe_tier`).
pub fn get_next_probe_tier(current_length: i64) -> Option<i64> {
    CONTEXT_PROBE_TIERS.iter().copied().find(|tier| *tier < current_length)
}

static CONTEXT_LIMIT_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    // parse_context_limit_from_error patterns (model_metadata.py:1249-1257).
    [
        r"max_model_len\s*(?:is\s*)?[:=(]?\s*(\d{4,})",
        r"maximum model length\s*(?:is\s*)?[:=(]?\s*(\d{4,})",
        r"(?:max(?:imum)?|limit)\s*(?:context\s*)?(?:length|size|window)?\s*(?:is|of|:)?\s*(\d{4,})",
        r"context\s*(?:length|size|window)\s*(?:is|of|:)?\s*(\d{4,})",
        r"(\d{4,})\s*(?:token)?\s*(?:context|limit)",
        r">\s*(\d{4,})\s*(?:max|limit|token)",
        r"(\d{4,})\s*(?:max(?:imum)?)\b",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

/// Try to extract the actual context limit from an API error message
/// (`parse_context_limit_from_error`). Values outside [1024, 10M] are
/// rejected as noise.
pub fn parse_context_limit_from_error(error_msg: &str) -> Option<i64> {
    let error_lower = error_msg.to_lowercase();
    for pattern in CONTEXT_LIMIT_PATTERNS.iter() {
        if let Some(caps) = pattern.captures(&error_lower) {
            if let Some(m) = caps.get(1) {
                if let Ok(limit) = m.as_str().parse::<i64>() {
                    if (1024..=10_000_000).contains(&limit) {
                        return Some(limit);
                    }
                }
            }
        }
    }
    None
}

/// Return a provider-reported LOWER context limit, if one is present
/// (`get_context_length_from_provider_error`): never invent a window — only
/// step down when the provider explicitly reported a smaller maximum.
pub fn get_context_length_from_provider_error(
    error_msg: &str,
    current_context_length: i64,
) -> Option<i64> {
    let parsed = parse_context_limit_from_error(error_msg)?;
    if parsed < current_context_length {
        Some(parsed)
    } else {
        None
    }
}

/// Detect an "output cap too large" error and return how many output tokens
/// are available (`parse_available_output_tokens_from_error`). None when the
/// error does not look like an output-cap error.
pub fn parse_available_output_tokens_from_error(error_msg: &str) -> Option<i64> {
    let e = error_msg.to_lowercase();

    let is_output_cap_error = (e.contains("max_tokens")
        && (e.contains("available_tokens") || e.contains("available tokens")))
        || (e.contains("in the output") && e.contains("maximum context length"))
        || (e.contains("maximum context length")
            && e.contains("requested")
            && e.contains("output tokens"))
        || e.contains("range of max_tokens should be");
    if !is_output_cap_error {
        return None;
    }

    static RANGE_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"range of max_tokens should be\s*\[\s*\d+\s*,\s*(\d+)\s*\]").unwrap()
    });
    if let Some(c) = RANGE_RE.captures(&e).and_then(|c| c.get(1)) {
        if let Ok(cap) = c.as_str().parse::<i64>() {
            if cap >= 1 {
                return Some(cap);
            }
        }
    }

    static AVAIL_RES: Lazy<Vec<Regex>> = Lazy::new(|| {
        [
            r"available_tokens[:\s]+(\d+)",
            r"available\s+tokens[:\s]+(\d+)",
            r"=\s*(\d+)\s*$",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    });
    for re in AVAIL_RES.iter() {
        if let Some(c) = re.captures(&e).and_then(|c| c.get(1)) {
            if let Ok(tokens) = c.as_str().parse::<i64>() {
                if tokens >= 1 {
                    return Some(tokens);
                }
            }
        }
    }

    static CTX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"maximum context length is (\d+)").unwrap());
    static PARTS_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\((\d+)\s+of text input,\s*(\d+)\s+of tool input,\s*(\d+)\s+in the output\)")
            .unwrap()
    });
    if let (Some(ctx), Some(parts)) = (CTX_RE.captures(&e), PARTS_RE.captures(&e)) {
        let ctx: i64 = ctx.get(1)?.as_str().parse().ok()?;
        let text: i64 = parts.get(1)?.as_str().parse().ok()?;
        let tool: i64 = parts.get(2)?.as_str().parse().ok()?;
        let available = ctx - text - tool;
        if available >= 1 {
            return Some(available);
        }
    }

    static CTX_TOK_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"maximum context length is (\d+)\s*token").unwrap());
    static CHARS_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"prompt contains (\d+)\s*character").unwrap());
    if let (Some(ctx), Some(chars)) = (CTX_TOK_RE.captures(&e), CHARS_RE.captures(&e)) {
        let ctx: i64 = ctx.get(1)?.as_str().parse().ok()?;
        let chars: i64 = chars.get(1)?.as_str().parse().ok()?;
        let est_input = (chars + 2) / 3;
        let available = ctx - est_input;
        if available >= 1 {
            return Some(available);
        }
    }

    static VLLM_INPUT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"prompt contains (?:at least )?(\d+)\s*input tokens").unwrap());
    if let (Some(ctx), Some(input)) = (CTX_TOK_RE.captures(&e), VLLM_INPUT_RE.captures(&e)) {
        let ctx: i64 = ctx.get(1)?.as_str().parse().ok()?;
        let input: i64 = input.get(1)?.as_str().parse().ok()?;
        let available = ctx - input;
        if available >= 1 {
            return Some(available);
        }
    }

    None
}

/// Whether an error message is output-cap-shaped even without a parseable
/// budget (`is_output_cap_error` in error_classifier — the loop uses this to
/// exempt output-cap errors from compression).
pub fn is_output_cap_error(error_msg: &str) -> bool {
    parse_available_output_tokens_from_error(error_msg).is_some() || {
        let e = error_msg.to_lowercase();
        e.contains("max_tokens")
            && (e.contains("available_tokens")
                || e.contains("available tokens")
                || e.contains("range of max_tokens should be"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_longest_key_first() {
        assert_eq!(get_model_context_length("claude-sonnet-4-6", None), 1_000_000);
        assert_eq!(get_model_context_length("claude-3-opus", None), 200_000);
        assert_eq!(get_model_context_length("anthropic/claude-fable-5", None), 1_000_000);
        assert_eq!(get_model_context_length("gpt-5.5-codex", None), 1_050_000);
        assert_eq!(get_model_context_length("gpt-5-mini", None), 400_000);
        assert_eq!(get_model_context_length("minimax-m3", None), 1_000_000);
        assert_eq!(get_model_context_length("MiniMax-M2.5", None), 204_800);
        assert_eq!(get_model_context_length("unknown-model-xyz", None), DEFAULT_FALLBACK_CONTEXT);
        // Config override always wins.
        assert_eq!(get_model_context_length("claude-fable-5", Some(123_456)), 123_456);
    }

    #[test]
    fn provider_prefix_stripping() {
        assert_eq!(strip_provider_prefix("local:my-model"), "my-model");
        assert_eq!(strip_provider_prefix("qwen3.5:27b"), "qwen3.5:27b");
        assert_eq!(strip_provider_prefix("qwen:0.5b"), "qwen:0.5b");
        assert_eq!(strip_provider_prefix("deepseek:latest"), "deepseek:latest");
        assert_eq!(strip_provider_prefix("openrouter:z-ai/glm-5.2"), "z-ai/glm-5.2");
    }

    #[test]
    fn context_limit_parsing_realistic() {
        // Anthropic-style
        assert_eq!(
            parse_context_limit_from_error(
                "prompt is too long: 250000 tokens > 200000 maximum context length"
            ),
            Some(200_000)
        );
        // OpenAI-style
        assert_eq!(
            parse_context_limit_from_error(
                "This model's maximum context length is 128000 tokens. However, your messages resulted in 143222 tokens."
            ),
            Some(128_000)
        );
        // vLLM
        assert_eq!(
            parse_context_limit_from_error("max_model_len is 32768 tokens"),
            Some(32_768)
        );
        // No limit reported
        assert_eq!(
            parse_context_limit_from_error("the input exceeds the context window"),
            None
        );
        // Lower-only rule
        assert_eq!(
            get_context_length_from_provider_error("maximum context length is 128000 tokens", 200_000),
            Some(128_000)
        );
        assert_eq!(
            get_context_length_from_provider_error("maximum context length is 400000 tokens", 200_000),
            None
        );
    }

    #[test]
    fn output_cap_parsing() {
        assert_eq!(
            parse_available_output_tokens_from_error(
                "max_tokens: 32768 > context_window: 200000 - input_tokens: 190000 = available_tokens: 10000"
            ),
            Some(10_000)
        );
        assert_eq!(
            parse_available_output_tokens_from_error("Range of max_tokens should be [1, 65536]"),
            Some(65_536)
        );
        assert_eq!(parse_available_output_tokens_from_error("prompt is too long"), None);
    }
}
