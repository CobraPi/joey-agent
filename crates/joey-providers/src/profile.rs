//! Declarative per-provider profiles + registry (port of `providers/base.py`,
//! `providers/__init__.py`, the bundled `plugins/model-providers/*` profiles,
//! and the `hermes_cli/auth.py` provider registry entries).
//!
//! Only the providers actually ported to this crate are registered:
//! openrouter, anthropic, openai-api, nous, deepseek, gemini, zai, xai,
//! and GitHub Copilot.

use std::collections::HashMap;

use once_cell::sync::Lazy;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiMode {
    /// OpenAI Chat Completions (the default).
    ChatCompletions,
    /// Anthropic Messages API.
    AnthropicMessages,
    /// OpenAI Responses / Codex wire (upstream `codex_responses`).
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
        profile!(
            "copilot",
            &["github-copilot", "github-models", "github-model", "github"],
            ApiMode::ChatCompletions,
            "https://api.githubcopilot.com",
            &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
            Some("COPILOT_API_BASE_URL"),
            "",
            "GitHub Copilot",
            "GitHub Copilot (OAuth or fine-grained token)",
            "https://github.com/settings/copilot",
            &[]
        ),
        // Local AI Usage HUD reverse proxy (~/Development/ai-usage-hud): a
        // Copilot-compatible MITM proxy that owns upstream GitHub auth, token
        // refresh, and usage capture. Same wire protocol + credential
        // resolution as the copilot profile, but pinned to the local proxy's
        // endpoint (env-overridable). A joey-specific provider — no upstream
        // Hermes equivalent.
        profile!(
            "ai-usage-hud",
            &["usage-hud", "ai-usage"],
            ApiMode::ChatCompletions,
            "http://127.0.0.1:8317",
            &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
            Some("AI_USAGE_HUD_BASE_URL"),
            "",
            "AI Usage HUD",
            "AI Usage HUD (local Copilot reverse proxy w/ usage tracking)",
            "http://127.0.0.1:8317/",
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
        // codex_responses.
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

/// Providers that speak the GitHub Copilot wire protocol (Copilot headers,
/// credential resolution, model-id normalization, Responses routing). This is
/// the single source of truth for "copilot-family" dispatch — every call site
/// that used to hardcode `profile.name == "copilot"` must go through here so
/// new Copilot-compatible providers (e.g. the AI Usage HUD reverse proxy)
/// can't drift from the registry.
pub fn is_copilot_wire(profile_name: &str) -> bool {
    profile_name == "copilot" || profile_name == "ai-usage-hud"
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
    "copilot", "github-copilot", "github-models", "github-model", "github",
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

    // Custom Copilot-compatible endpoint (e.g. a local reverse proxy serving
    // the full Copilot catalog). Every auto-detection path below — hostname,
    // vendor prefix, bare model family — would re-route models to their
    // vendor-native endpoints and silently bypass the proxy. Instead, route
    // EVERYTHING through the matching copilot-wire profile: `build_client`
    // pins its base URL to the custom endpoint, and the proxy serves every
    // model family.
    if crate::copilot::hud_endpoint().is_some() {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("ai-usage-hud").unwrap();
    }
    if crate::copilot::custom_endpoint().is_some() {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("copilot").unwrap();
    }

    // Detect from base_url hostname.
    let host = joey_core::utils::base_url_hostname(base_url);
    if host.contains("openrouter.ai") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("openrouter").unwrap();
    }
    if host.contains("api.anthropic.com") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("anthropic").unwrap();
    }
    if host.contains("api.openai.com") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("openai-api").unwrap();
    }
    if host == "api.githubcopilot.com" || host.ends_with(".githubcopilot.com") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("copilot").unwrap();
    }
    if host.contains("nousresearch.com") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("nous").unwrap();
    }
    // Exact/suffix matching for short ambiguous domains: `contains("x.ai")`
    // misrouted e.g. max.ai/box.ai/flex.ai hosts onto the xai profile.
    if host == "x.ai" || host.ends_with(".x.ai") || host == "api.x.ai" {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("xai").unwrap();
    }
    if host == "z.ai" || host.ends_with(".z.ai") || host == "api.z.ai" {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("zai").unwrap();
    }
    if host.contains("deepseek.com") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("deepseek").unwrap();
    }
    if host.contains("googleapis.com") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("gemini").unwrap();
    }

    // Detect from model prefix (`anthropic/claude-...`).
    if let Some((prefix, _)) = model.split_once('/') {
        if KNOWN_PREFIXES.contains(&prefix) {
            if prefix == "google" {
                // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
                return get_profile("gemini").unwrap();
            }
            if let Some(p) = get_profile(prefix) {
                return p;
            }
        }
    }
    if model.starts_with("claude-") && host.contains("anthropic") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("anthropic").unwrap();
    }

    // Detect from bare model family name when no host/prefix was recognized.
    // This matters for OMO agent switching: when a fallback chain resolves a
    // bare model ID like "glm-5.2", switch_model forwards provider="auto" with
    // an empty base_url. Without this family-level detection the model would
    // fall through to the OpenRouter aggregator below — but GLM models have a
    // native Z.AI provider that should be used directly. Match the same family
    // prefixes that ModelFamily::detect knows about.
    let lower = model.to_ascii_lowercase();
    if lower.starts_with("glm-") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("zai").unwrap();
    }
    if lower.starts_with("claude-") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("anthropic").unwrap();
    }
    if lower.starts_with("gpt-") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("openai-api").unwrap();
    }
    if lower.starts_with("gemini-") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("gemini").unwrap();
    }
    if lower.starts_with("deepseek-") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("deepseek").unwrap();
    }
    if lower.starts_with("grok-") {
        // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
        return get_profile("xai").unwrap();
    }

    // Fall back to OpenRouter (the aggregator default) — many custom
    // OpenAI-compatible endpoints land here with a base_url override.
    // SAFETY: hardcoded provider alias mapped at build time; profile is guaranteed to exist.
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
    if is_copilot_wire(profile.name) {
        return crate::copilot::normalize_model_id(model);
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
    fn copilot_profile_and_wire_names_resolve() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        let p = resolve_profile("github-copilot", "", "github-copilot/gpt-5.4");
        assert_eq!(p.name, "copilot");
        for alias in ["github-models", "github-model", "github"] {
            assert_eq!(get_profile(alias).unwrap().name, "copilot");
        }
        assert_eq!(wire_model_name(&p, "github-copilot/gpt-5.4"), "gpt-5.4");
        assert_eq!(resolve_profile("auto", "https://api.githubcopilot.com", "gpt-4.1").name, "copilot");
    }

    #[test]
    fn resolves_by_base_url() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        let p = resolve_profile("auto", "https://api.anthropic.com", "claude-opus-4.6");
        assert_eq!(p.name, "anthropic");
        assert_eq!(p.api_mode, ApiMode::AnthropicMessages);
    }

    #[test]
    fn resolves_by_model_prefix() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        let p = resolve_profile("auto", "https://openrouter.ai/api/v1", "anthropic/claude-opus-4.6");
        assert_eq!(p.name, "openrouter");
    }

    /// Bare GLM model names (no provider prefix, no base_url) resolve to the
    /// native zai provider, not the OpenRouter aggregator. This is the OMO
    /// agent-switch path: switch_model forwards provider="auto" with an empty
    /// base_url, and the fallback chain produces bare IDs like "glm-5.2".
    #[test]
    fn bare_glm_resolves_to_zai() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        let p = resolve_profile("auto", "", "glm-5.2");
        assert_eq!(p.name, "zai");
        assert_eq!(p.base_url, "https://api.z.ai/api/paas/v4");
        // GLM-4.6v (vision variant) also routes natively
        assert_eq!(resolve_profile("auto", "", "glm-4.6v").name, "zai");
        assert_eq!(resolve_profile("auto", "", "glm-5").name, "zai");
    }

    /// Bare model names for other families resolve to their native providers
    /// instead of falling through to the aggregator default.
    #[test]
    fn bare_family_names_resolve_to_native_providers() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        assert_eq!(resolve_profile("auto", "", "claude-opus-4-8").name, "anthropic");
        assert_eq!(resolve_profile("auto", "", "gpt-5.6-sol").name, "openai-api");
        assert_eq!(resolve_profile("auto", "", "gemini-3.1-pro").name, "gemini");
        assert_eq!(resolve_profile("auto", "", "deepseek-chat").name, "deepseek");
        assert_eq!(resolve_profile("auto", "", "grok-4").name, "xai");
    }

    /// Explicit host detection still wins over bare-name family detection:
    /// a base_url pointing at z.ai with a glm model returns zai even though
    /// both paths agree — and pointing at openrouter.ai keeps it openrouter.
    #[test]
    fn host_detection_takes_priority_over_bare_name() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        // Host match wins and is not masked by the bare-name fallback.
        let p = resolve_profile("auto", "https://api.z.ai/api/paas/v4", "glm-5.2");
        assert_eq!(p.name, "zai");
        // Explicit OpenRouter host with a glm model → openrouter (aggregator).
        let or = resolve_profile("auto", "https://openrouter.ai/api/v1", "glm-5.2");
        assert_eq!(or.name, "openrouter");
    }

    /// Custom Copilot-compatible endpoint (reverse proxy): when
    /// COPILOT_API_BASE_URL points off githubcopilot.com, EVERY auto-detection
    /// path resolves to the copilot profile so no request escapes the proxy —
    /// vendor prefixes, bare family names, and foreign base_url hosts alike.
    #[test]
    fn custom_endpoint_magnetizes_all_auto_resolution() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("COPILOT_API_BASE_URL", "http://127.0.0.1:8317");
        // Bare family names that would otherwise route natively.
        assert_eq!(resolve_profile("auto", "", "claude-opus-4-8").name, "copilot");
        assert_eq!(resolve_profile("auto", "", "glm-5.2").name, "copilot");
        assert_eq!(resolve_profile("auto", "", "gpt-5.6-sol").name, "copilot");
        assert_eq!(resolve_profile("auto", "", "gemini-3.1-pro").name, "copilot");
        assert_eq!(resolve_profile("auto", "", "kimi-k3").name, "copilot");
        // Vendor-prefixed models.
        assert_eq!(
            resolve_profile("auto", "", "anthropic/claude-opus-4.6").name,
            "copilot"
        );
        // Foreign base_url hosts would otherwise win host detection.
        assert_eq!(
            resolve_profile("auto", "https://api.z.ai/api/paas/v4", "glm-5.2").name,
            "copilot"
        );
        assert_eq!(
            resolve_profile("auto", "https://api.anthropic.com", "claude-opus-4.6").name,
            "copilot"
        );
        // An explicit non-auto provider setting still wins (the user forced a
        // different backend) — only "auto" resolution is magnetized.
        assert_eq!(resolve_profile("zai", "", "glm-5.2").name, "zai");
        std::env::remove_var("COPILOT_API_BASE_URL");
    }

    /// Without the custom endpoint, native routing is untouched.
    #[test]
    fn no_custom_endpoint_keeps_native_resolution() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        assert_eq!(resolve_profile("auto", "", "glm-5.2").name, "zai");
        assert_eq!(resolve_profile("auto", "", "claude-opus-4.8").name, "anthropic");
    }

    /// ai-usage-hud: a first-class Copilot-wire provider that pins the local
    /// reverse proxy. Registry rows must be distinct from copilot's.
    #[test]
    fn ai_usage_hud_profile_registers_with_copilot_wire_semantics() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        let hud = get_profile("ai-usage-hud").unwrap();
        assert_eq!(hud.name, "ai-usage-hud");
        assert_eq!(hud.base_url, "http://127.0.0.1:8317");
        assert_eq!(hud.base_url_env_var, Some("AI_USAGE_HUD_BASE_URL"));
        assert_eq!(hud.env_vars, &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]);
        assert_eq!(hud.api_mode, ApiMode::ChatCompletions);
        // Aliases resolve to the same canonical profile.
        assert_eq!(get_profile("usage-hud").unwrap().name, "ai-usage-hud");
        assert_eq!(get_profile("ai-usage").unwrap().name, "ai-usage-hud");
        // Copilot-family dispatch includes it (single source of truth).
        assert!(is_copilot_wire("ai-usage-hud"));
        assert!(is_copilot_wire("copilot"));
        assert!(!is_copilot_wire("zai"));
        // Distinct registry row from copilot (non-collision).
        assert_ne!(get_profile("copilot").unwrap().base_url, hud.base_url);
        // Wire model names normalize through the copilot rules.
        assert_eq!(wire_model_name(&hud, "copilot/gpt-5.4"), "gpt-5.4");
        assert_eq!(wire_model_name(&hud, "openai/o3"), "gpt-5.3-codex");
    }

    /// Explicit `ai-usage-hud` provider setting resolves to the profile with
    /// the proxy base URL pinned — no env var required.
    #[test]
    fn ai_usage_hud_explicit_setting_pins_proxy() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        let p = resolve_profile("ai-usage-hud", "", "claude-sonnet-4.6");
        assert_eq!(p.name, "ai-usage-hud");
        assert_eq!(p.base_url, "http://127.0.0.1:8317");
        // The alias works as an explicit setting too.
        assert_eq!(resolve_profile("usage-hud", "", "").name, "ai-usage-hud");
    }

    /// AI_USAGE_HUD_BASE_URL magnetizes auto-resolution to the ai-usage-hud
    /// profile (same semantics as COPILOT_API_BASE_URL → copilot).
    #[test]
    fn hud_env_var_magnetizes_auto_resolution() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://127.0.0.1:8317");
        assert_eq!(resolve_profile("auto", "", "glm-5.2").name, "ai-usage-hud");
        assert_eq!(resolve_profile("auto", "", "claude-opus-4.8").name, "ai-usage-hud");
        assert_eq!(
            resolve_profile("auto", "https://api.z.ai/api/paas/v4", "glm-5.2").name,
            "ai-usage-hud"
        );
        // An explicit different provider still wins.
        assert_eq!(resolve_profile("zai", "", "glm-5.2").name, "zai");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
    }

    /// The HUD env var pointing at the real Copilot host is ignored (keeps
    /// the exchange flow) — mirrors the COPILOT_API_BASE_URL guard.
    #[test]
    fn hud_env_var_ignores_real_copilot_host() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "https://api.githubcopilot.com");
        assert!(crate::copilot::hud_endpoint().is_none());
        assert_eq!(resolve_profile("auto", "", "glm-5.2").name, "zai");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
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
