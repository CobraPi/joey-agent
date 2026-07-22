//! `joey-providers` — the LLM provider layer for joey-agent.
//!
//! Port of `providers/`, `agent/transports/`, and the wire adapters. Maps a
//! provider-neutral [`ProviderRequest`] onto either the OpenAI Chat Completions
//! or Anthropic Messages wire protocol, with SSE streaming, and normalizes the
//! result into a [`NormalizedResponse`].

pub mod anthropic;
pub mod chat;
pub mod client;
pub mod error;
pub mod profile;
pub mod request;
pub mod types;

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
    let profile = resolve_profile(provider_setting, base_url, model);
    let base_override = resolve_base_override(&profile, base_url);
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
