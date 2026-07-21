//! Declarative per-provider profiles + registry (port of `providers/base.py`
//! and `providers/__init__.py`).

use std::collections::HashMap;

use once_cell::sync::Lazy;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiMode {
    /// OpenAI Chat Completions (the default; ~16 providers).
    ChatCompletions,
    /// Anthropic Messages API.
    AnthropicMessages,
}

impl ApiMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiMode::ChatCompletions => "chat_completions",
            ApiMode::AnthropicMessages => "anthropic_messages",
        }
    }
}

/// How a provider authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    OAuth,
}

/// A declarative provider profile.
#[derive(Debug, Clone)]
pub struct ProviderProfile {
    pub name: &'static str,
    pub api_mode: ApiMode,
    pub base_url: &'static str,
    /// Env vars that may hold the API key, in priority order.
    pub env_vars: &'static [&'static str],
    pub auth_type: AuthType,
    pub supports_vision: bool,
    /// Default output-token cap when the caller doesn't specify one.
    pub default_max_tokens: u32,
    /// Default auxiliary (side-task) model for this provider.
    pub default_aux_model: &'static str,
    /// Extra static headers (name, value) to attach to every request.
    pub default_headers: &'static [(&'static str, &'static str)],
}

impl ProviderProfile {
    /// Resolve the API key for this provider from the environment.
    pub fn resolve_api_key(&self) -> Option<String> {
        for var in self.env_vars {
            if let Ok(v) = std::env::var(var) {
                let t = v.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }
}

macro_rules! profile {
    ($name:expr, $mode:expr, $url:expr, $envs:expr, $vision:expr, $max:expr, $aux:expr) => {
        ProviderProfile {
            name: $name,
            api_mode: $mode,
            base_url: $url,
            env_vars: $envs,
            auth_type: AuthType::ApiKey,
            supports_vision: $vision,
            default_max_tokens: $max,
            default_aux_model: $aux,
            default_headers: &[],
        }
    };
}

static PROFILES: Lazy<HashMap<&'static str, ProviderProfile>> = Lazy::new(|| {
    let mut m = HashMap::new();
    let list = [
        profile!(
            "openrouter",
            ApiMode::ChatCompletions,
            "https://openrouter.ai/api/v1",
            &["OPENROUTER_API_KEY"],
            true,
            16384,
            "google/gemini-3-flash-preview"
        ),
        profile!(
            "anthropic",
            ApiMode::AnthropicMessages,
            "https://api.anthropic.com",
            &["ANTHROPIC_API_KEY"],
            true,
            32000,
            "claude-haiku-4-5"
        ),
        profile!(
            "openai",
            ApiMode::ChatCompletions,
            "https://api.openai.com/v1",
            &["OPENAI_API_KEY"],
            true,
            16384,
            "gpt-5-mini"
        ),
        profile!(
            "nous",
            ApiMode::ChatCompletions,
            "https://inference-api.nousresearch.com/v1",
            &["NOUS_API_KEY", "NOUS_ACCESS_TOKEN"],
            true,
            16384,
            "gemini-3-flash"
        ),
        profile!(
            "deepseek",
            ApiMode::ChatCompletions,
            "https://api.deepseek.com/v1",
            &["DEEPSEEK_API_KEY"],
            false,
            8192,
            "deepseek-chat"
        ),
        profile!(
            "groq",
            ApiMode::ChatCompletions,
            "https://api.groq.com/openai/v1",
            &["GROQ_API_KEY"],
            false,
            8192,
            "llama-3.3-70b-versatile"
        ),
        profile!(
            "gemini",
            ApiMode::ChatCompletions,
            "https://generativelanguage.googleapis.com/v1beta/openai",
            &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            true,
            16384,
            "gemini-3-flash"
        ),
        profile!(
            "xai",
            ApiMode::ChatCompletions,
            "https://api.x.ai/v1",
            &["XAI_API_KEY"],
            true,
            16384,
            "grok-4-fast"
        ),
        profile!(
            "zai",
            ApiMode::ChatCompletions,
            "https://api.z.ai/api/paas/v4",
            &["GLM_API_KEY", "ZAI_API_KEY"],
            false,
            16384,
            "glm-4.6"
        ),
        profile!(
            "ollama",
            ApiMode::ChatCompletions,
            "http://localhost:11434/v1",
            &["OLLAMA_API_KEY"],
            false,
            8192,
            "llama3.2"
        ),
    ];
    for p in list {
        m.insert(p.name, p);
    }
    m
});

/// Look up a provider profile by canonical name.
pub fn get_profile(name: &str) -> Option<ProviderProfile> {
    PROFILES.get(name).cloned()
}

/// All known provider names, sorted.
pub fn provider_names() -> Vec<&'static str> {
    let mut names: Vec<_> = PROFILES.keys().copied().collect();
    names.sort_unstable();
    names
}

/// Known provider prefixes that can appear in a `provider/model` string.
const KNOWN_PREFIXES: &[&str] = &[
    "anthropic", "openai", "google", "openrouter", "groq", "mistral", "xai", "deepseek",
    "nous", "gemini", "zai", "ollama",
];

/// Resolve which provider profile to use, given an explicit provider setting
/// (may be "auto"), the base_url, and the model string. Mirrors upstream's
/// base-url-hostname + model-prefix detection.
pub fn resolve_profile(provider_setting: &str, base_url: &str, model: &str) -> ProviderProfile {
    let setting = provider_setting.trim();
    if !setting.is_empty() && setting != "auto" {
        if let Some(p) = get_profile(setting) {
            return p;
        }
    }

    // Detect from base_url hostname.
    let host = joey_core::utils::base_url_hostname(base_url);
    if host.contains("openrouter.ai") {
        return get_profile("openrouter").unwrap();
    }
    if host.contains("api.anthropic.com") {
        return get_profile("anthropic").unwrap();
    }
    if host.contains("api.openai.com") {
        return get_profile("openai").unwrap();
    }
    if host.contains("nousresearch.com") {
        return get_profile("nous").unwrap();
    }
    if host.contains("x.ai") {
        return get_profile("xai").unwrap();
    }
    if host.contains("z.ai") {
        return get_profile("zai").unwrap();
    }
    if host.contains("deepseek.com") {
        return get_profile("deepseek").unwrap();
    }
    if host.contains("groq.com") {
        return get_profile("groq").unwrap();
    }
    if host.contains("googleapis.com") {
        return get_profile("gemini").unwrap();
    }

    // Detect from model prefix (`anthropic/claude-...`).
    if let Some((prefix, _)) = model.split_once('/') {
        if KNOWN_PREFIXES.contains(&prefix) {
            if let Some(p) = get_profile(prefix) {
                return p;
            }
            if prefix == "google" {
                return get_profile("gemini").unwrap();
            }
        }
        // `claude-*` bare models imply Anthropic wire when base is anthropic.
    }
    if model.starts_with("claude-") && host.contains("anthropic") {
        return get_profile("anthropic").unwrap();
    }

    // Fall back to OpenRouter (the aggregator default), but keep the caller's
    // base_url — many custom OpenAI-compatible endpoints land here.
    get_profile("openrouter").unwrap()
}

/// Strip a leading known provider prefix from a model string for wire use.
/// OpenRouter wants `anthropic/claude-...`; native providers want `claude-...`.
pub fn wire_model_name(profile: &ProviderProfile, model: &str) -> String {
    if profile.name == "openrouter" {
        return model.to_string();
    }
    if let Some((prefix, rest)) = model.split_once('/') {
        if KNOWN_PREFIXES.contains(&prefix) {
            return rest.to_string();
        }
    }
    model.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_by_base_url() {
        let p = resolve_profile("auto", "https://api.anthropic.com", "claude-opus-4.6");
        assert_eq!(p.name, "anthropic");
        assert_eq!(p.api_mode, ApiMode::AnthropicMessages);
    }

    #[test]
    fn resolves_by_model_prefix() {
        let p = resolve_profile("auto", "https://openrouter.ai/api/v1", "anthropic/claude-opus-4.6");
        assert_eq!(p.name, "openrouter");
    }

    #[test]
    fn explicit_setting_wins() {
        let p = resolve_profile("openai", "https://openrouter.ai/api/v1", "gpt-5");
        assert_eq!(p.name, "openai");
    }

    #[test]
    fn wire_name_strips_prefix_for_native() {
        let anthropic = get_profile("anthropic").unwrap();
        assert_eq!(wire_model_name(&anthropic, "anthropic/claude-opus-4.6"), "claude-opus-4.6");
        let openrouter = get_profile("openrouter").unwrap();
        assert_eq!(wire_model_name(&openrouter, "anthropic/claude-opus-4.6"), "anthropic/claude-opus-4.6");
    }
}
