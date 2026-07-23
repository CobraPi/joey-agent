//! `joey-providers` — the LLM provider layer for joey-agent.
//!
//! Port of `providers/`, `agent/transports/`, and the wire adapters. Maps a
//! provider-neutral [`ProviderRequest`] onto either the OpenAI Chat Completions
//! or Anthropic Messages wire protocol, with SSE streaming, and normalizes the
//! result into a [`NormalizedResponse`].

pub mod anthropic;
pub mod chat;
pub mod client;
pub mod copilot;
pub mod error;
pub mod profile;
pub mod request;
pub mod types;
pub mod zai;

pub use client::ProviderClient;
pub use error::{jittered_backoff, jittered_backoff_api, jittered_backoff_with, parse_retry_after, ProviderError};
pub use profile::{resolve_profile, ApiMode, AuthType, ProviderProfile};
pub use request::{ProviderRequest, ReasoningEffort};
pub use types::{
    ContentPart, FinishReason, FunctionCall, ImageUrl, Message, NormalizedResponse, StreamEvent,
    ToolCall, ToolSchema, Usage,
};

/// The app-wide default base URL (the OpenRouter aggregator). Callers that
/// leave `model.base_url` at this default must NOT hijack a non-OpenRouter
/// provider's own endpoint.
const DEFAULT_AGGREGATOR_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Build a client from a resolved provider setting, base URL, and model.
/// Convenience wrapper over [`resolve_profile`] + [`ProviderClient::new`].
pub fn build_client(
    provider_setting: &str,
    base_url: &str,
    model: &str,
    api_key: Option<String>,
) -> Result<ProviderClient, ProviderError> {
    let mut profile = resolve_profile(provider_setting, base_url, model);
    if profile.name == "copilot" {
        // Hermes consults the live catalog for non-GPT models because Claude
        // may expose only /v1/messages while Gemini/older models use chat.
        let normalized = copilot::normalize_model_id(model);
        let catalog = copilot::fetch_model_catalog(std::time::Duration::from_secs(5)).ok();
        let entry = catalog.as_ref().and_then(|items| {
            items.iter().find(|item| {
                item.get("id").and_then(serde_json::Value::as_str) == Some(normalized.as_str())
            })
        });
        profile.api_mode = copilot::model_api_mode(model, entry);
    }
    let mut base_override = resolve_base_override(&profile, base_url);
    // Z.AI: with no explicit override (env var or config base_url), resolve
    // the endpoint by probing — global vs China vs Coding Plan billing paths
    // accept different keys (auth.py `resolve_api_key_provider_credentials`
    // → `_resolve_zai_base_url`; cached in auth.json after the first probe).
    if base_override.is_none() && profile.name == "zai" {
        let key = api_key
            .clone()
            .or_else(|| profile.resolve_api_key())
            .unwrap_or_default();
        // env_override is "" here: resolve_base_override already returned
        // any GLM_BASE_URL value as Some above.
        let resolved = zai::resolve_zai_base_url(&key, profile.base_url, "");
        if resolved != profile.base_url {
            base_override = Some(resolved);
        }
    }
    ProviderClient::new(profile, base_override, api_key)
}

/// Resolve the effective base-URL override for a provider (M3): honor explicit
/// overrides for EVERY provider — the `<ID>_BASE_URL` env var
/// (auth.py `base_url_env_var`) or a caller-supplied base_url — not just
/// openrouter/custom. The app-wide default aggregator URL is treated as "no
/// override" for non-OpenRouter providers so leaving `model.base_url` at its
/// default doesn't hijack a native provider's own endpoint (agent_init.py:947-1005).
fn resolve_base_override(profile: &ProviderProfile, base_url: &str) -> Option<String> {
    // Env-var override takes precedence (auth.py base_url_env_var).
    if let Some(env_base) = profile.resolve_base_url_env() {
        return Some(env_base);
    }
    let b = base_url.trim();
    if b.is_empty() || b == profile.base_url {
        return None;
    }
    // The aggregator default must not override a non-aggregator provider.
    if b == DEFAULT_AGGREGATOR_BASE_URL && profile.name != "openrouter" {
        return None;
    }
    Some(b.to_string())
}
