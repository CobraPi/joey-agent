//! Declarative per-provider profiles + registry (port of `providers/base.py`,
//! `providers/__init__.py`, the bundled `plugins/model-providers/*` profiles,
//! and the `hermes_cli/auth.py` provider registry entries).
//!
//! Only the providers actually ported to this crate are registered:
//! openrouter, anthropic, openai-api, nous, deepseek, gemini, zai, xai.

use std::collections::HashMap;

use once_cell::sync::Lazy;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiMode {
    /// OpenAI Chat Completions (the default).
    ChatCompletions,
    /// Anthropic Messages API.
    AnthropicMessages,
    /// OpenAI Responses / Codex wire (upstream `codex_responses`). Not yet
    /// ported — building a client for such a profile returns an error rather
    /// than silently remapping onto a different wire.
    CodexResponses,
}

impl ApiMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiMode::ChatCompletions => "chat_completions",
            ApiMode::AnthropicMessages => "anthropic_messages",
            ApiMode::CodexResponses => "codex_responses",
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
    /// Alternate names accepted anywhere a provider id is (providers/__init__.py:53-73).
    pub aliases: &'static [&'static str],
    pub api_mode: ApiMode,
    pub base_url: &'static str,
    /// Env vars that may hold the API key, in priority order.
    pub env_vars: &'static [&'static str],
    /// Env var that overrides the base URL for this provider, when upstream
    /// defines one (auth.py `base_url_env_var`).
    pub base_url_env_var: Option<&'static str>,
    pub auth_type: AuthType,
    /// Default output-token cap when the caller doesn't specify one. Upstream
    /// `ProviderProfile.default_max_tokens` — None for every ported provider
    /// (the Anthropic-family model table is the only fallback; see
    /// chat_completions.py:563-580).
    pub default_max_tokens: Option<u32>,
    /// Default auxiliary (side-task) model for this provider ("" = none).
    pub default_aux_model: &'static str,
    /// Extra static headers (name, value) to attach to every request.
    pub default_headers: &'static [(&'static str, &'static str)],
    /// Short display name (auth.py `ProviderConfig.name` /
    /// models.py `ProviderEntry.label`) — shown in picker rows and labels.
    pub display_name: &'static str,
    /// Longer picker description (models.py `ProviderEntry.tui_desc`).
    pub tui_desc: &'static str,
    /// Signup / key page shown during setup (plugin `signup_url`; "" = none).
    pub signup_url: &'static str,
    /// Curated fallback model ids shown when live fetch fails (plugin
    /// `fallback_models`). Only agentic tool-calling models belong here.
    pub fallback_models: &'static [&'static str],
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

    /// Resolve a base-URL override from the environment (`<X>_BASE_URL`),
    /// when this provider defines one (auth.py `base_url_env_var`).
    pub fn resolve_base_url_env(&self) -> Option<String> {
        let var = self.base_url_env_var?;
        let v = std::env::var(var).ok()?;
        let t = v.trim();
        (!t.is_empty()).then(|| t.to_string())
    }
}

macro_rules! profile {
    ($name:expr, $aliases:expr, $mode:expr, $url:expr, $envs:expr, $burl_env:expr, $aux:expr,
     $display:expr, $tui:expr, $signup:expr, $fallback:expr) => {
        ProviderProfile {
            name: $name,
            aliases: $aliases,
            api_mode: $mode,
            base_url: $url,
            env_vars: $envs,
            base_url_env_var: $burl_env,
            auth_type: AuthType::ApiKey,
            default_max_tokens: None,
            default_aux_model: $aux,
            default_headers: &[],
            display_name: $display,
            tui_desc: $tui,
            signup_url: $signup,
            fallback_models: $fallback,
        }
    };
}

static PROFILES: Lazy<HashMap<&'static str, ProviderProfile>> = Lazy::new(|| {
    let mut m = HashMap::new();
    // Metadata sources per provider: plugin `__init__.py` (signup_url,
    // fallback_models, aux model), auth.py PROVIDER_REGISTRY
    // (base_url_env_var, display name), models.py CANONICAL_PROVIDERS
    // (label + tui_desc shown by the `joey model` picker).
    let list = [
        // plugins/model-providers/openrouter/__init__.py:170-186
        profile!(
            "openrouter",
            &["or"],
            ApiMode::ChatCompletions,
            "https://openrouter.ai/api/v1",
            &["OPENROUTER_API_KEY"],
            None,
            "",
            "OpenRouter",
            "OpenRouter (Pay-per-use API aggregator)",
            "https://openrouter.ai/keys",
            &[
                "anthropic/claude-sonnet-4.6",
                "openai/gpt-5.4",
                "deepseek/deepseek-chat",
                "google/gemini-3-flash-preview",
                "qwen/qwen3-plus"
            ]
        ),
        // plugins/model-providers/anthropic/__init__.py:44-52
        profile!(
            "anthropic",
            &["claude", "claude-oauth", "claude-code"],
            ApiMode::AnthropicMessages,
            "https://api.anthropic.com",
            &["ANTHROPIC_API_KEY", "ANTHROPIC_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"],
            Some("ANTHROPIC_BASE_URL"),
            "claude-haiku-4-5-20251001",
            "Anthropic",
            "Anthropic (Claude models via API key or Claude Code)",
            "https://platform.claude.com/settings/keys",
            &[]
        ),
        // hermes_cli/auth.py:192-199 ("openai-api"). "openai" kept as an
        // alias because upstream accepts it as a provider setting elsewhere
        // (hermes_cli/runtime_provider.py:390).
        profile!(
            "openai-api",
            &["openai"],
            ApiMode::ChatCompletions,
            "https://api.openai.com/v1",
            &["OPENAI_API_KEY"],
            Some("OPENAI_BASE_URL"),
            "",
            "OpenAI API",
            "OpenAI API (api.openai.com, API key)",
            "",
            &[]
        ),
        // plugins/model-providers/nous/__init__.py:43-58. Upstream auth_type
        // is oauth_device_code; the device-code OAuth flow is not ported, so
        // auth stays ApiKey (NOUS_API_KEY) here — deliberate adaptation.
        profile!(
            "nous",
            &["nous-portal", "nousresearch"],
            ApiMode::ChatCompletions,
            "https://inference-api.nousresearch.com/v1",
            &["NOUS_API_KEY"],
            None,
            "",
            "Nous Portal",
            "Nous Portal (Everything your agent needs, 300+ models with bundled tool use)",
            "https://nousresearch.com/",
            &["hermes-3-405b", "hermes-3-70b"]
        ),
        // plugins/model-providers/deepseek/__init__.py:85-98
        profile!(
            "deepseek",
            &["deepseek-chat"],
            ApiMode::ChatCompletions,
            "https://api.deepseek.com/v1",
            &["DEEPSEEK_API_KEY"],
            Some("DEEPSEEK_BASE_URL"),
            "deepseek-chat",
            "DeepSeek",
            "DeepSeek (V3, R1, coder, direct API)",
            "https://platform.deepseek.com/",
            &["deepseek-chat", "deepseek-reasoner"]
        ),
        // plugins/model-providers/gemini/__init__.py:51-59. Upstream's gemini
        // profile uses a NATIVE Gemini REST adapter (GeminiNativeClient at
        // base https://generativelanguage.googleapis.com/v1beta); that
        // adapter is unported, so this profile keeps Google's OpenAI-compat
        // /openai shim as its base URL — deliberate adaptation. Env order
        // matches upstream: GOOGLE_API_KEY first, then GEMINI_API_KEY.
        profile!(
            "gemini",
            &["google", "google-gemini", "google-ai-studio"],
            ApiMode::ChatCompletions,
            "https://generativelanguage.googleapis.com/v1beta/openai",
            &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
            Some("GEMINI_BASE_URL"),
            "gemini-3.5-flash",
            "Google AI Studio",
            "Google AI Studio (Native Gemini API)",
            "",
            &[]
        ),
        // plugins/model-providers/zai/__init__.py:111-125 +
        // auth.py PROVIDER_REGISTRY["zai"] (GLM_BASE_URL override).
        profile!(
            "zai",
            &["glm", "z-ai", "z.ai", "zhipu"],
            ApiMode::ChatCompletions,
            "https://api.z.ai/api/paas/v4",
            &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"],
            Some("GLM_BASE_URL"),
            "glm-4.5-flash",
            "Z.AI / GLM",
            "Z.AI / GLM (Zhipu direct API)",
            "https://z.ai/",
            &["glm-5.2", "glm-5", "glm-4-9b"]
        ),
        // plugins/model-providers/xai/__init__.py — upstream api_mode is
        // codex_responses. The codex wire is not ported; building a client
        // for this profile fails with a clear error instead of silently
        // remapping to chat_completions.
        profile!(
            "xai",
            &["grok", "x-ai", "x.ai"],
            ApiMode::CodexResponses,
            "https://api.x.ai/v1",
            &["XAI_API_KEY"],
            Some("XAI_BASE_URL"),
            "",
            "xAI",
            "xAI Grok (Direct API)",
            "",
            &[]
        ),
    ];
    for p in list {
        m.insert(p.name, p);
    }
    m
});

/// Alias → canonical-name map, built from the profiles' alias lists
/// (providers/__init__.py:53-63).
static ALIASES: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    for p in PROFILES.values() {
        for alias in p.aliases {
            m.insert(*alias, p.name);
        }
    }
    m
});

/// Look up a provider profile by canonical name or alias
/// (providers/__init__.py:65-73).
pub fn get_profile(name: &str) -> Option<ProviderProfile> {
    let canonical = ALIASES.get(name).copied().unwrap_or(name);
    PROFILES.get(canonical).cloned()
}

/// All known provider names, sorted.
pub fn provider_names() -> Vec<&'static str> {
    let mut names: Vec<_> = PROFILES.keys().copied().collect();
    names.sort_unstable();
    names
}

/// Known provider prefixes that can appear in a `provider/model` string.
const KNOWN_PREFIXES: &[&str] = &[
    "anthropic", "openai", "google", "openrouter", "xai", "deepseek", "nous", "gemini", "zai",
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
        return get_profile("openai-api").unwrap();
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
    if host.contains("googleapis.com") {
        return get_profile("gemini").unwrap();
    }

    // Detect from model prefix (`anthropic/claude-...`).
    if let Some((prefix, _)) = model.split_once('/') {
        if KNOWN_PREFIXES.contains(&prefix) {
            if prefix == "google" {
                return get_profile("gemini").unwrap();
            }
            if let Some(p) = get_profile(prefix) {
                return p;
            }
        }
    }
    if model.starts_with("claude-") && host.contains("anthropic") {
        return get_profile("anthropic").unwrap();
    }

    // Fall back to OpenRouter (the aggregator default) — many custom
    // OpenAI-compatible endpoints land here with a base_url override.
    get_profile("openrouter").unwrap()
}

/// The model name to put on the wire for `profile`.
///
/// - OpenRouter keeps the full `vendor/model` slug.
/// - The Anthropic wire applies upstream `normalize_model_name`
///   (anthropic_adapter.py:1605-1631): strip `anthropic/`, dots→hyphens for
///   `claude-*` models, Bedrock IDs preserved.
/// - Other native providers strip a known vendor prefix.
pub fn wire_model_name(profile: &ProviderProfile, model: &str) -> String {
    if profile.name == "openrouter" {
        return model.to_string();
    }
    if profile.api_mode == ApiMode::AnthropicMessages {
        return crate::anthropic::normalize_model_name(model);
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
        let p = resolve_profile("openai-api", "https://openrouter.ai/api/v1", "gpt-5");
        assert_eq!(p.name, "openai-api");
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(get_profile("claude").unwrap().name, "anthropic");
        assert_eq!(get_profile("claude-code").unwrap().name, "anthropic");
        assert_eq!(get_profile("glm").unwrap().name, "zai");
        assert_eq!(get_profile("z.ai").unwrap().name, "zai");
        assert_eq!(get_profile("or").unwrap().name, "openrouter");
        assert_eq!(get_profile("openai").unwrap().name, "openai-api");
        assert_eq!(get_profile("grok").unwrap().name, "xai");
        assert_eq!(get_profile("google").unwrap().name, "gemini");
        assert_eq!(get_profile("nous-portal").unwrap().name, "nous");
    }

    #[test]
    fn invented_providers_removed() {
        assert!(get_profile("groq").is_none());
        assert!(get_profile("ollama").is_none());
    }

    #[test]
    fn registry_composition_matches_upstream() {
        let a = get_profile("anthropic").unwrap();
        assert_eq!(
            a.env_vars,
            &["ANTHROPIC_API_KEY", "ANTHROPIC_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"]
        );
        assert_eq!(a.default_aux_model, "claude-haiku-4-5-20251001");
        let g = get_profile("gemini").unwrap();
        assert_eq!(g.env_vars, &["GOOGLE_API_KEY", "GEMINI_API_KEY"]);
        assert_eq!(g.default_aux_model, "gemini-3.5-flash");
        let z = get_profile("zai").unwrap();
        assert_eq!(z.env_vars, &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"]);
        assert_eq!(z.default_aux_model, "glm-4.5-flash");
        // zai picker metadata (zai/__init__.py:111-125 + auth.py registry +
        // models.py CANONICAL_PROVIDERS).
        assert_eq!(z.base_url_env_var, Some("GLM_BASE_URL"));
        assert_eq!(z.display_name, "Z.AI / GLM");
        assert_eq!(z.tui_desc, "Z.AI / GLM (Zhipu direct API)");
        assert_eq!(z.signup_url, "https://z.ai/");
        assert_eq!(z.fallback_models, &["glm-5.2", "glm-5", "glm-4-9b"]);
        let n = get_profile("nous").unwrap();
        assert_eq!(n.env_vars, &["NOUS_API_KEY"]);
        assert_eq!(n.default_aux_model, "");
        let o = get_profile("openrouter").unwrap();
        assert_eq!(o.default_aux_model, "");
        let oa = get_profile("openai-api").unwrap();
        assert_eq!(oa.base_url_env_var, Some("OPENAI_BASE_URL"));
        assert_eq!(oa.default_aux_model, "");
        let x = get_profile("xai").unwrap();
        assert_eq!(x.api_mode, ApiMode::CodexResponses);
        assert_eq!(x.base_url_env_var, Some("XAI_BASE_URL"));
        // base_url_env_var per auth.py PROVIDER_REGISTRY.
        assert_eq!(a.base_url_env_var, Some("ANTHROPIC_BASE_URL"));
        assert_eq!(g.base_url_env_var, Some("GEMINI_BASE_URL"));
        assert_eq!(get_profile("deepseek").unwrap().base_url_env_var, Some("DEEPSEEK_BASE_URL"));
        assert_eq!(o.base_url_env_var, None);
        assert_eq!(n.base_url_env_var, None);
        // No invented per-provider output caps.
        for name in provider_names() {
            let p = get_profile(name).unwrap();
            assert_eq!(p.default_max_tokens, None);
            // Every provider carries picker metadata (label + tui_desc).
            assert!(!p.display_name.is_empty(), "{} missing display_name", name);
            assert!(!p.tui_desc.is_empty(), "{} missing tui_desc", name);
        }
    }

    #[test]
    fn wire_name_normalizes_for_anthropic() {
        let anthropic = get_profile("anthropic").unwrap();
        // normalize_model_name: strip prefix AND dots→hyphens (H2).
        assert_eq!(wire_model_name(&anthropic, "anthropic/claude-opus-4.6"), "claude-opus-4-6");
        assert_eq!(wire_model_name(&anthropic, "claude-sonnet-4.6"), "claude-sonnet-4-6");
        let openrouter = get_profile("openrouter").unwrap();
        assert_eq!(
            wire_model_name(&openrouter, "anthropic/claude-opus-4.6"),
            "anthropic/claude-opus-4.6"
        );
    }
}
